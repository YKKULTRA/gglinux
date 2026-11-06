// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl screen`: what the keyboard's screen shows, and its lines in `ggctl keyboard`.

use anyhow::{bail, Context, Result};
use gg_ipc::{KeyboardState, MonitorItem, Request, Screen};

pub fn request(rest: &[&str]) -> Result<Request> {
    let screen = match rest.split_first() {
        Some((&"keyboard", [])) => Screen::Keyboard,
        Some((&"clock", options)) => {
            if let Some(o) = options
                .iter()
                .find(|o| !matches!(**o, "seconds" | "12h" | "date"))
            {
                bail!("unknown clock option '{o}' (try: seconds, 12h, date)");
            }
            Screen::Clock {
                seconds: options.contains(&"seconds"),
                h24: !options.contains(&"12h"),
                date: options.contains(&"date"),
            }
        }
        Some((&"text", words)) if !words.is_empty() => Screen::Text {
            text: words.join(" ").replace('|', "\n"),
        },
        Some((&"image", [path])) => {
            let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
            Screen::Image {
                pixels: parse_pbm(&bytes).with_context(|| format!("{path} is not a 128x40 PBM"))?,
            }
        }
        Some((&"gif", [path])) => {
            let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
            let (screen, cut) = from_gif(&bytes).with_context(|| {
                format!("{path} is not a GIF ggctl can read (the app also reads APNG and WebP)")
            })?;
            if cut {
                eprintln!(
                    "note: {path} has more than {} pictures; only the first {} are kept",
                    Screen::MAX_FRAMES,
                    Screen::MAX_FRAMES
                );
            }
            screen
        }
        Some((&"monitor", [list])) => Screen::Monitor {
            items: list.split(',').map(reading).collect::<Result<_>>()?,
        },
        _ => bail!(
            "usage: ggctl screen keyboard | clock [seconds] [12h] [date] | text ... | \
             image file.pbm | gif file.gif | monitor cpu,temp,ram,gpu,gpu-temp,clock"
        ),
    };
    screen.validate().map_err(anyhow::Error::msg)?;
    Ok(Request::SetScreen { screen })
}

/// A System Monitor reading by its name on the command line.
fn reading(name: &str) -> Result<MonitorItem> {
    Ok(match name {
        "cpu" => MonitorItem::Cpu,
        "temp" | "cpu-temp" => MonitorItem::CpuTemp,
        "ram" | "memory" => MonitorItem::Ram,
        "gpu" => MonitorItem::Gpu,
        "gpu-temp" => MonitorItem::GpuTemp,
        "clock" | "time" => MonitorItem::Clock,
        other => bail!("unknown reading '{other}' (try: cpu, temp, ram, gpu, gpu-temp, clock)"),
    })
}

/// A 128x40 PBM, plain (`P1`) or raw (`P4`), as 640 bytes of rows: 16 a row, the leftmost
/// pixel in each byte's high bit, 1 lit — PBM's own raster layout.
fn parse_pbm(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut pos = 0;
    // The next header token, skipping whitespace and `#` comments.
    let mut token = || -> Result<String> {
        loop {
            match bytes.get(pos) {
                Some(b'#') => {
                    while bytes.get(pos).is_some_and(|&b| b != b'\n') {
                        pos += 1;
                    }
                }
                Some(b) if b.is_ascii_whitespace() => pos += 1,
                Some(_) => break,
                None => bail!("the header ends early"),
            }
        }
        let start = pos;
        while bytes.get(pos).is_some_and(|b| !b.is_ascii_whitespace()) {
            pos += 1;
        }
        Ok(String::from_utf8_lossy(&bytes[start..pos]).into_owned())
    };
    let magic = token()?;
    let (w, h) = (token()?, token()?);
    if (w.as_str(), h.as_str()) != ("128", "40") {
        bail!("it is {w}x{h}");
    }
    match magic.as_str() {
        "P4" => {
            let raster = bytes.get(pos + 1..).unwrap_or_default();
            if raster.len() < 640 {
                bail!("its raster is {} bytes, not 640", raster.len());
            }
            Ok(raster[..640].to_vec())
        }
        "P1" => {
            let bits: Vec<bool> = bytes[pos..]
                .iter()
                .filter_map(|b| match b {
                    b'0' => Some(false),
                    b'1' => Some(true),
                    _ => None,
                })
                .collect();
            if bits.len() < 128 * 40 {
                bail!("it has {} pixels, not {}", bits.len(), 128 * 40);
            }
            Ok(bits[..128 * 40]
                .chunks(8)
                .map(|c| c.iter().fold(0u8, |acc, &on| (acc << 1) | u8::from(on)))
                .collect())
        }
        other => bail!("'{other}' is not a PBM (P1 or P4)"),
    }
}

/// A GIF as the screen shows it: an animation of its first [`Screen::MAX_FRAMES`]
/// pictures, or a still picture if it has just one; and whether pictures were left out.
/// Each picture is turned into 1-bit pixels as the app does with its defaults (threshold
/// 128, dithered), and each delay is pulled into the 0.1 to 10 s the screen allows.
///
/// The desktop app reads GIF, APNG and WebP with the `image` crate. ggctl doesn't link it
/// (it isn't a dependency of this package), so this is a small GIF reader of its own and a
/// copy of the app's dithering. The app scales with a triangle filter and this with a
/// plain average, so a picture can differ by a pixel here and there.
fn from_gif(bytes: &[u8]) -> Result<(Screen, bool)> {
    let mut frames = Vec::new();
    let mut delays_ms = Vec::new();
    let cut = gif::decode(bytes, Screen::MAX_FRAMES, |canvas, w, h, delay_ms| {
        frames.push(to_rows(canvas, w, h));
        let range = Screen::FRAME_DELAY_MS;
        let ms = delay_ms.clamp(u32::from(*range.start()), u32::from(*range.end()));
        delays_ms.push(ms as u16);
    })?;
    Ok(match frames.len() {
        1 => (
            Screen::Image {
                pixels: frames.remove(0),
            },
            cut,
        ),
        _ => (Screen::Animation { frames, delays_ms }, cut),
    })
}

const W: usize = 128;
const H: usize = 40;

/// `w` × `h` RGBA pixels fitted into 128 × 40 on black (an average of the pixels each
/// screen pixel covers), then dithered at threshold 128 into 640 bytes of rows.
fn to_rows(px: &[[u8; 4]], iw: usize, ih: usize) -> Vec<u8> {
    let scale = (W as f32 / iw as f32).min(H as f32 / ih as f32);
    let w = ((iw as f32 * scale).round() as usize).clamp(1, W);
    let h = ((ih as f32 * scale).round() as usize).clamp(1, H);
    let (ox, oy) = ((W - w) / 2, (H - h) / 2);
    // Which source pixels the `t`th of `to` covers, out of `from`: at least one.
    let span = |t: usize, to: usize, from: usize| {
        let a = (t * from / to).min(from - 1);
        (a, ((t + 1) * from / to).clamp(a + 1, from))
    };
    let mut lum = vec![0f32; W * H];
    for ty in 0..h {
        let (y0, y1) = span(ty, h, ih);
        for tx in 0..w {
            let (x0, x1) = span(tx, w, iw);
            let mut sum = 0f32;
            for y in y0..y1 {
                for p in &px[y * iw + x0..y * iw + x1] {
                    // Transparent parts are on black.
                    let l = 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]);
                    sum += l * f32::from(p[3]) / 255.0;
                }
            }
            lum[(ty + oy) * W + tx + ox] = sum / ((y1 - y0) * (x1 - x0)) as f32;
        }
    }
    let mut rows = vec![0u8; Screen::IMAGE_BYTES];
    for y in 0..H {
        for x in 0..W {
            // Floyd–Steinberg, as the app's.
            let i = y * W + x;
            let on = lum[i] >= 128.0;
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
            if on {
                rows[y * 16 + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    rows
}

/// Just enough of GIF89a to play one: the colour tables, each picture's delay,
/// transparency and disposal, interlacing, and LZW.
mod gif {
    use anyhow::{bail, ensure, Context, Result};

    /// The most pixels a GIF's canvas may have here: a 4K screen's worth.
    const MOST_PIXELS: usize = 3840 * 2160;

    struct Reader<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Result<&'a [u8]> {
            let end = self.pos.checked_add(n).filter(|&e| e <= self.bytes.len());
            let end = end.context("the file ends early")?;
            let out = &self.bytes[self.pos..end];
            self.pos = end;
            Ok(out)
        }

        fn u8(&mut self) -> Result<u8> {
            Ok(self.take(1)?[0])
        }

        fn u16(&mut self) -> Result<usize> {
            let b = self.take(2)?;
            Ok(usize::from(u16::from_le_bytes([b[0], b[1]])))
        }

        /// Data sub-blocks up to their terminator, joined.
        fn blocks(&mut self) -> Result<Vec<u8>> {
            let mut data = Vec::new();
            loop {
                let n = usize::from(self.u8()?);
                if n == 0 {
                    return Ok(data);
                }
                data.extend_from_slice(self.take(n)?);
            }
        }

        /// A colour table of the size `packed`'s low bits give.
        fn colours(&mut self, packed: u8) -> Result<Vec<[u8; 3]>> {
            let n = 2usize << (packed & 7);
            Ok(self
                .take(3 * n)?
                .chunks(3)
                .map(|c| [c[0], c[1], c[2]])
                .collect())
        }
    }

    /// What a graphic control extension says about the next picture.
    #[derive(Default)]
    struct Control {
        delay_ms: u32,
        transparent: Option<u8>,
        /// 2: cleared after it shows; 3: what was there before comes back.
        disposal: u8,
    }

    /// Each picture of `bytes` as the whole canvas shows it (RGBA, row by row, `w` × `h`)
    /// and its delay in ms, up to `most` of them; true if there were more.
    pub fn decode(
        bytes: &[u8],
        most: usize,
        mut each: impl FnMut(&[[u8; 4]], usize, usize, u32),
    ) -> Result<bool> {
        let mut r = Reader { bytes, pos: 0 };
        let magic = r.take(6)?;
        ensure!(magic == b"GIF87a" || magic == b"GIF89a", "it doesn't start like a GIF");
        let (w, h) = (r.u16()?, r.u16()?);
        ensure!(w > 0 && h > 0, "it's {w}x{h}");
        ensure!(w * h <= MOST_PIXELS, "it's {w}x{h}, too big to read here");
        let packed = r.u8()?;
        r.take(2)?; // background colour, aspect
        let global = if packed & 0x80 != 0 {
            Some(r.colours(packed)?)
        } else {
            None
        };
        let mut canvas = vec![[0u8; 4]; w * h];
        let mut control = Control::default();
        let mut shown = 0;
        // A file cut short after a whole picture still plays what it has.
        while let Ok(block) = r.u8() {
            match block {
                0x21 => {
                    let label = r.u8()?;
                    let data = r.blocks()?;
                    if label == 0xF9 && data.len() >= 4 {
                        control = Control {
                            delay_ms: u32::from(u16::from_le_bytes([data[1], data[2]])) * 10,
                            transparent: (data[0] & 1 != 0).then_some(data[3]),
                            disposal: (data[0] >> 2) & 7,
                        };
                    }
                }
                0x2C => {
                    if shown == most {
                        return Ok(true);
                    }
                    let (left, top, fw, fh) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?);
                    let packed = r.u8()?;
                    let local = if packed & 0x80 != 0 {
                        Some(r.colours(packed)?)
                    } else {
                        None
                    };
                    let colours = local
                        .as_ref()
                        .or(global.as_ref())
                        .context("a picture has no colour table")?;
                    let min = r.u8()?;
                    let data = r.blocks()?;
                    let indices = lzw(&data, min, fw * fh)?;
                    let interlaced = packed & 0x40 != 0;
                    let before = (control.disposal == 3).then(|| canvas.clone());
                    for y in 0..fh {
                        let row = if interlaced { source_row(y, fh) } else { y };
                        for x in 0..fw {
                            let (cx, cy) = (left + x, top + y);
                            let Some(&i) = indices.get(row * fw + x) else {
                                continue;
                            };
                            if cx >= w || cy >= h || Some(i) == control.transparent {
                                continue;
                            }
                            let [red, green, blue] =
                                colours.get(usize::from(i)).copied().unwrap_or_default();
                            canvas[cy * w + cx] = [red, green, blue, 255];
                        }
                    }
                    each(&canvas, w, h, control.delay_ms);
                    shown += 1;
                    match control.disposal {
                        2 => {
                            for y in top..(top + fh).min(h) {
                                for x in left..(left + fw).min(w) {
                                    canvas[y * w + x] = [0; 4];
                                }
                            }
                        }
                        3 => canvas = before.unwrap_or(canvas),
                        _ => {}
                    }
                    control = Control::default();
                }
                0x3B => break,
                other => bail!("an unknown block {other:#04x}"),
            }
        }
        ensure!(shown > 0, "it has no pictures");
        Ok(false)
    }

    /// Where row `y` of an interlaced picture `h` rows high comes in its data: every 8th
    /// row from 0, then every 8th from 4, every 4th from 2, every 2nd from 1.
    pub fn source_row(y: usize, h: usize) -> usize {
        const PASSES: [(usize, usize); 4] = [(0, 8), (4, 8), (2, 4), (1, 2)];
        let rows_in = |(start, step): (usize, usize)| (h + step - 1).saturating_sub(start) / step;
        let mut before = 0;
        for pass in PASSES {
            let (start, step) = pass;
            if y >= start && (y - start) % step == 0 {
                return before + (y - start) / step;
            }
            before += rows_in(pass);
        }
        unreachable!("every row is in a pass")
    }

    /// GIF's LZW: codes from `min + 1` to 12 bits, least significant bit first, with a
    /// clear code and an end code. Gives at most `pixels` colour indices.
    pub fn lzw(data: &[u8], min: u8, pixels: usize) -> Result<Vec<u8>> {
        ensure!((1..=8).contains(&min), "a code size of {min}");
        let clear = 1usize << min;
        let end = clear + 1;
        let mut prefix = vec![0u16; 4096];
        let mut suffix = vec![0u8; 4096];
        let mut size = u32::from(min) + 1;
        let mut next = end + 1;
        let mut prev: Option<usize> = None;
        let (mut acc, mut bits) = (0u32, 0u32);
        let mut out = Vec::with_capacity(pixels);
        let mut string = Vec::new();
        for &byte in data {
            acc |= u32::from(byte) << bits;
            bits += 8;
            while bits >= size {
                let code = (acc & ((1 << size) - 1)) as usize;
                acc >>= size;
                bits -= size;
                if code == clear {
                    size = u32::from(min) + 1;
                    next = end + 1;
                    prev = None;
                    continue;
                }
                if code == end {
                    return Ok(out);
                }
                let Some(p) = prev else {
                    ensure!(code < clear, "the first code after a clear isn't a colour");
                    out.push(code as u8);
                    prev = Some(code);
                    continue;
                };
                // The string for `code`; one not in the table yet is the last one plus
                // its own first colour.
                let start = match code {
                    c if c < next => c,
                    c if c == next => p,
                    _ => bail!("a code that isn't in the table"),
                };
                string.clear();
                let mut c = start;
                while c > end {
                    string.push(suffix[c]);
                    c = usize::from(prefix[c]);
                }
                ensure!(c < clear, "a broken table");
                let head = c as u8;
                out.push(head);
                out.extend(string.iter().rev());
                if code == next {
                    out.push(head);
                }
                if next < 4096 {
                    prefix[next] = p as u16;
                    suffix[next] = head;
                    next += 1;
                    if next == 1 << size && size < 12 {
                        size += 1;
                    }
                }
                prev = Some(code);
                if out.len() >= pixels {
                    out.truncate(pixels);
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }
}

/// How `ggctl keyboard` names a screen setting.
fn describe(screen: &Screen) -> String {
    match screen {
        Screen::Keyboard => "the keyboard's own".to_owned(),
        Screen::Text { text } => format!("text {:?}", text),
        Screen::Clock { .. } => "clock".to_owned(),
        Screen::Image { .. } => "picture".to_owned(),
        Screen::Animation { frames, delays_ms } => {
            let ms: u64 = delays_ms.iter().map(|&d| u64::from(d)).sum();
            format!(
                "animation of {} pictures, {:.1} s",
                frames.len(),
                ms as f64 / 1000.0
            )
        }
        Screen::Monitor { items } => {
            let names: Vec<&str> = items
                .iter()
                .map(|i| match i {
                    MonitorItem::Cpu => "CPU use",
                    MonitorItem::CpuTemp => "CPU temperature",
                    MonitorItem::Ram => "memory",
                    MonitorItem::Gpu => "GPU use",
                    MonitorItem::GpuTemp => "GPU temperature",
                    MonitorItem::Clock => "clock",
                })
                .collect();
            format!("system monitor: {}", names.join(", "))
        }
    }
}

/// The screen and screen care lines of `ggctl keyboard`.
pub fn print(state: &KeyboardState) {
    println!("screen:    {}", describe(&state.screen));
    if let Some(e) = &state.screen_error {
        println!("           the keyboard refused it: {e}");
    }
    let c = &state.screen_care;
    let on = |b: bool| if b { "on" } else { "off" };
    println!(
        "care:      shift {}; dark while away {}; the keyboard's own too {}{}",
        on(c.shift),
        on(c.dark_when_away),
        on(c.own_screen_too),
        if state.screen_resting {
            " (resting now: nobody's at the computer)"
        } else {
            ""
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args, parse, sends};

    #[test]
    fn screen_commands_build_the_screen() {
        match sends("screen clock seconds date") {
            Request::SetScreen {
                screen: Screen::Clock { seconds, h24, date },
            } => assert!(seconds && h24 && date),
            other => panic!("{other:?}"),
        }
        match sends("screen text GG on|Linux") {
            Request::SetScreen {
                screen: Screen::Text { text },
            } => assert_eq!(text, "GG on\nLinux"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            sends("screen keyboard"),
            Request::SetScreen {
                screen: Screen::Keyboard
            }
        ));
        assert!(parse(&args("screen clock fast")).is_err());
        assert!(parse(&args("screen text this line is far too long")).is_err());
    }

    #[test]
    fn a_pbm_becomes_640_bytes_of_rows() {
        let mut raw = b"P4\n# a comment\n128 40\n".to_vec();
        let mut raster = vec![0u8; 640];
        raster[0] = 0x80;
        raw.extend_from_slice(&raster);
        assert_eq!(parse_pbm(&raw).unwrap(), raster);

        let mut plain = String::from("P1\n128 40\n1");
        plain.push_str(&"0".repeat(128 * 40 - 1));
        let rows = parse_pbm(plain.as_bytes()).unwrap();
        assert_eq!(rows.len(), 640);
        assert_eq!(rows[0], 0x80);
        assert!(rows[1..].iter().all(|&b| b == 0));

        assert!(parse_pbm(b"P4\n64 32\n").is_err(), "wrong size");
        assert!(parse_pbm(b"P5\n128 40\n255\n").is_err(), "greyscale");
    }

    #[test]
    fn the_monitor_takes_up_to_four_readings_by_name() {
        match sends("screen monitor cpu,ram,temp,clock") {
            Request::SetScreen {
                screen: Screen::Monitor { items },
            } => assert_eq!(
                items,
                [
                    MonitorItem::Cpu,
                    MonitorItem::Ram,
                    MonitorItem::CpuTemp,
                    MonitorItem::Clock
                ]
            ),
            other => panic!("{other:?}"),
        }
        match sends("screen monitor gpu,gpu-temp") {
            Request::SetScreen {
                screen: Screen::Monitor { items },
            } => assert_eq!(items, [MonitorItem::Gpu, MonitorItem::GpuTemp]),
            other => panic!("{other:?}"),
        }
        for bad in [
            "screen monitor cpu,fan",
            "screen monitor cpu,ram,temp,clock,gpu",
            "screen monitor cpu,cpu",
            "screen monitor",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
        let Err(unknown) = parse(&args("screen monitor fan")) else {
            panic!("an unknown reading is refused")
        };
        let unknown = unknown.to_string();
        assert!(unknown.contains("try: cpu, temp"), "{unknown}");
    }

    #[test]
    fn keyboard_lists_an_animation_and_the_monitor_plainly() {
        let animation = Screen::Animation {
            frames: vec![vec![0; 640]; 12],
            delays_ms: vec![100; 12],
        };
        assert_eq!(describe(&animation), "animation of 12 pictures, 1.2 s");
        let monitor = Screen::Monitor {
            items: vec![MonitorItem::Cpu, MonitorItem::Ram, MonitorItem::Clock],
        };
        assert_eq!(describe(&monitor), "system monitor: CPU use, memory, clock");
    }

    /// Three 20 × 10 pictures from Pillow (real LZW, a colour table each): pixel (x, y)
    /// of picture i is white where (x + y + i) % 3 == 0; 50, 200 and 300 ms.
    const STRIPES: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x14, 0x00, 0x0a, 0x00, 0x81, 0x00, 0x00, 0xff, 0xff,
        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x21, 0xff, 0x0b, 0x4e, 0x45,
        0x54, 0x53, 0x43, 0x41, 0x50, 0x45, 0x32, 0x2e, 0x30, 0x03, 0x01, 0x00, 0x00, 0x00, 0x21,
        0xf9, 0x04, 0x00, 0x05, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x14, 0x00, 0x0a,
        0x00, 0x00, 0x08, 0x2c, 0x00, 0x01, 0x04, 0x08, 0x20, 0x90, 0xe0, 0xc0, 0x82, 0x08, 0x0f,
        0x0e, 0x34, 0xc8, 0x30, 0x61, 0xc3, 0x85, 0x0e, 0x23, 0x2a, 0x24, 0x28, 0xf1, 0xa1, 0x45,
        0x8b, 0x15, 0x0b, 0x62, 0x9c, 0x18, 0x31, 0x23, 0xc7, 0x89, 0x1b, 0x43, 0x86, 0xcc, 0xe8,
        0x71, 0x63, 0x40, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x14, 0x00, 0x02, 0x00, 0x2c, 0x00, 0x00,
        0x00, 0x00, 0x14, 0x00, 0x0a, 0x00, 0x81, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x08, 0x2f, 0x00, 0x03, 0x08, 0x00, 0x20, 0x90, 0xe0, 0xc0, 0x82,
        0x08, 0x0f, 0x0a, 0x50, 0x68, 0xb0, 0x61, 0xc2, 0x86, 0x0e, 0x19, 0x3e, 0x44, 0x38, 0x51,
        0xa2, 0x45, 0x8b, 0x11, 0x33, 0x66, 0xac, 0xe8, 0x90, 0xa3, 0xc7, 0x85, 0x1b, 0x31, 0x16,
        0x0c, 0x49, 0x32, 0xc0, 0xc7, 0x90, 0x01, 0x01, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x1e, 0x00,
        0x02, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x14, 0x00, 0x0a, 0x00, 0x81, 0xff, 0xff, 0xff,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x2f, 0x00, 0x05, 0x00, 0x08,
        0x20, 0x90, 0xe0, 0xc0, 0x82, 0x08, 0x0f, 0x02, 0x50, 0x68, 0xb0, 0x61, 0xc2, 0x86, 0x0e,
        0x19, 0x3e, 0x44, 0x38, 0x51, 0xa2, 0x45, 0x8b, 0x11, 0x33, 0x66, 0xac, 0xe8, 0x90, 0xa3,
        0xc7, 0x85, 0x1b, 0x31, 0x16, 0x0c, 0x49, 0x52, 0xc0, 0xc7, 0x90, 0x01, 0x01, 0x00, 0x3b,
    ];

    /// Three 24 × 8 pictures from Pillow, the 2nd and 3rd only the part that changed:
    /// a box at (1..5, 1..5); then another at (16..22, 2..7) too; then the first gone.
    const BOXES: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x18, 0x00, 0x08, 0x00, 0x81, 0x00, 0x00, 0xff, 0xff,
        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x21, 0xff, 0x0b, 0x4e, 0x45,
        0x54, 0x53, 0x43, 0x41, 0x50, 0x45, 0x32, 0x2e, 0x30, 0x03, 0x01, 0x00, 0x00, 0x00, 0x21,
        0xf9, 0x04, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x18, 0x00, 0x08,
        0x00, 0x00, 0x08, 0x22, 0x00, 0x03, 0x08, 0x1c, 0x48, 0xb0, 0xa0, 0x41, 0x82, 0x00, 0x12,
        0x02, 0x38, 0xc8, 0xb0, 0xa0, 0xc2, 0x85, 0x0d, 0x23, 0x3e, 0x8c, 0x28, 0x51, 0x21, 0xc5,
        0x8b, 0x18, 0x33, 0x6a, 0xdc, 0x18, 0x31, 0x20, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x0a, 0x00,
        0x02, 0x00, 0x2c, 0x10, 0x00, 0x02, 0x00, 0x06, 0x00, 0x05, 0x00, 0x81, 0xff, 0xff, 0xff,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x0c, 0x00, 0x01, 0x08, 0x1c,
        0x48, 0xb0, 0xa0, 0xc1, 0x83, 0x02, 0x03, 0x02, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x0a, 0x00,
        0x02, 0x00, 0x2c, 0x01, 0x00, 0x01, 0x00, 0x04, 0x00, 0x04, 0x00, 0x81, 0xff, 0xff, 0xff,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x09, 0x00, 0x03, 0x08, 0x1c,
        0x48, 0xb0, 0x60, 0x80, 0x80, 0x00, 0x3b,
    ];

    /// Every picture of a GIF as (canvas, width, height, delay).
    fn pictures(bytes: &[u8], most: usize) -> (Vec<(Vec<[u8; 4]>, usize, usize, u32)>, bool) {
        let mut all = Vec::new();
        let more = gif::decode(bytes, most, |c, w, h, d| all.push((c.to_vec(), w, h, d))).unwrap();
        (all, more)
    }

    fn white(p: [u8; 4]) -> bool {
        p == [255, 255, 255, 255]
    }

    #[test]
    fn pillow_s_gif_reads_back_picture_by_picture() {
        let (all, more) = pictures(STRIPES, 256);
        assert!(!more);
        assert_eq!(all.len(), 3);
        for (i, (canvas, w, h, _)) in all.iter().enumerate() {
            assert_eq!((*w, *h), (20, 10));
            for y in 0..10 {
                for x in 0..20 {
                    assert_eq!(
                        white(canvas[y * 20 + x]),
                        (x + y + i) % 3 == 0,
                        "picture {i} at ({x}, {y})"
                    );
                }
            }
        }
        let delays: Vec<u32> = all.iter().map(|p| p.3).collect();
        assert_eq!(delays, [50, 200, 300]);
    }

    #[test]
    fn a_picture_of_only_what_changed_is_drawn_over_the_last() {
        let (all, _) = pictures(BOXES, 256);
        assert_eq!(all.len(), 3);
        let boxed = |x: usize, y: usize, b: (usize, usize, usize, usize)| {
            (b.0..b.2).contains(&x) && (b.1..b.3).contains(&y)
        };
        let (a, b) = ((1, 1, 5, 5), (16, 2, 22, 7));
        for (i, (canvas, _, _, _)) in all.iter().enumerate() {
            for y in 0..8 {
                for x in 0..24 {
                    let want = match i {
                        0 => boxed(x, y, a),
                        1 => boxed(x, y, a) || boxed(x, y, b),
                        _ => boxed(x, y, b),
                    };
                    assert_eq!(white(canvas[y * 24 + x]), want, "picture {i} at ({x}, {y})");
                }
            }
        }
    }

    /// LZW data that never grows its table: a clear code comes before it would.
    fn plain_lzw(indices: &[u8], min: u8) -> Vec<u8> {
        let clear = 1u32 << min;
        let size = u32::from(min) + 1;
        let mut codes = vec![clear];
        for chunk in indices.chunks((clear - 2) as usize) {
            codes.extend(chunk.iter().map(|&i| u32::from(i)));
            codes.push(clear);
        }
        codes.push(clear + 1);
        let (mut out, mut acc, mut bits) = (Vec::new(), 0u64, 0);
        for c in codes {
            acc |= u64::from(c) << bits;
            bits += size;
            while bits >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        }
        if bits > 0 {
            out.push(acc as u8);
        }
        out
    }

    struct Part {
        at: (u16, u16, u16, u16),
        indices: Vec<u8>,
        delay_cs: u16,
        disposal: u8,
        transparent: Option<u8>,
        interlaced: bool,
    }

    /// A GIF with a 4-colour table (black, white, red, green) and these pictures.
    fn build(w: u16, h: u16, parts: &[Part]) -> Vec<u8> {
        let mut b = b"GIF89a".to_vec();
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&[0x81, 0, 0]);
        b.extend_from_slice(&[0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 255, 0]);
        for p in parts {
            let flags = (p.disposal << 2) | u8::from(p.transparent.is_some());
            let [d0, d1] = p.delay_cs.to_le_bytes();
            b.extend_from_slice(&[0x21, 0xF9, 4, flags, d0, d1, p.transparent.unwrap_or(0), 0]);
            b.push(0x2C);
            for v in [p.at.0, p.at.1, p.at.2, p.at.3] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.push(if p.interlaced { 0x40 } else { 0 });
            let (fw, fh) = (usize::from(p.at.2), usize::from(p.at.3));
            let mut indices = p.indices.clone();
            if p.interlaced {
                for y in 0..fh {
                    let row = gif::source_row(y, fh);
                    indices[row * fw..(row + 1) * fw].copy_from_slice(&p.indices[y * fw..(y + 1) * fw]);
                }
            }
            b.push(2);
            for chunk in plain_lzw(&indices, 2).chunks(255) {
                b.push(chunk.len() as u8);
                b.extend_from_slice(chunk);
            }
            b.push(0);
        }
        b.push(0x3B);
        b
    }

    fn part(at: (u16, u16, u16, u16), colour: u8) -> Part {
        Part {
            at,
            indices: vec![colour; usize::from(at.2) * usize::from(at.3)],
            delay_cs: 10,
            disposal: 0,
            transparent: None,
            interlaced: false,
        }
    }

    #[test]
    fn transparency_disposal_and_interlacing_are_followed() {
        // Interlaced rows, each its own colour, to see they land in order.
        let rows: Vec<u8> = (0..10u8).flat_map(|y| [y % 4; 3]).collect();
        let interlaced = Part {
            indices: rows,
            interlaced: true,
            ..part((0, 0, 3, 10), 0)
        };
        let (all, _) = pictures(&build(3, 10, &[interlaced]), 256);
        let colour = |p: [u8; 4]| match p {
            [0, 0, 0, 255] => 0,
            [255, 255, 255, 255] => 1,
            [255, 0, 0, 255] => 2,
            [0, 255, 0, 255] => 3,
            _ => 9,
        };
        let seen: Vec<u8> = (0..10).map(|y| colour(all[0].0[y * 3])).collect();
        assert_eq!(seen, [0, 1, 2, 3, 0, 1, 2, 3, 0, 1]);

        // White everywhere; red at (1, 0) with green see-through around it, then cleared
        // (disposal 2); green at (2, 0), then the white back (disposal 3).
        let mut window = part((0, 0, 3, 1), 3);
        window.indices[1] = 2;
        window.transparent = Some(3);
        window.disposal = 2;
        let mut restored = part((2, 0, 1, 1), 3);
        restored.disposal = 3;
        let file = build(
            3,
            1,
            &[part((0, 0, 3, 1), 1), window, restored, part((0, 0, 1, 1), 1)],
        );
        let (all, _) = pictures(&file, 256);
        let at = |n: usize| -> Vec<u8> { all[n].0.iter().map(|&p| colour(p)).collect() };
        assert_eq!(at(0), [1, 1, 1]);
        assert_eq!(at(1), [1, 2, 1], "green is see-through");
        assert_eq!(at(2), [9, 9, 3], "cleared to nothing, then green");
        assert_eq!(at(3), [1, 9, 9], "the clear came back, not the green");
    }

    #[test]
    fn a_gif_becomes_an_animation_of_at_most_256_pictures_with_its_delays_pulled_in() {
        let (screen, cut) = from_gif(STRIPES).unwrap();
        assert!(!cut);
        let Screen::Animation { frames, delays_ms } = &screen else {
            panic!("{screen:?}")
        };
        assert_eq!(frames.len(), 3);
        assert_eq!(delays_ms, &[100, 200, 300], "50 ms is 0.1 s");
        assert_eq!(screen.validate(), Ok(()));

        let many: Vec<Part> = (0..300).map(|i| part((0, 0, 4, 1), (i % 2) as u8)).collect();
        let (screen, cut) = from_gif(&build(4, 1, &many)).unwrap();
        assert!(cut, "said so");
        match screen {
            Screen::Animation { frames, delays_ms } => {
                assert_eq!((frames.len(), delays_ms.len()), (256, 256));
            }
            other => panic!("{other:?}"),
        }
        let mut slow = part((0, 0, 4, 1), 1);
        slow.delay_cs = 2000;
        let (screen, _) = from_gif(&build(4, 1, &[slow, part((0, 0, 4, 1), 0)])).unwrap();
        assert!(matches!(screen, Screen::Animation { delays_ms, .. } if delays_ms == [10_000, 100]));
    }

    #[test]
    fn one_picture_is_a_still_and_a_white_one_lights_the_screen() {
        let (screen, cut) = from_gif(&build(32, 10, &[part((0, 0, 32, 10), 1)])).unwrap();
        assert!(!cut);
        match screen {
            Screen::Image { pixels } => assert!(pixels.iter().all(|&b| b == 0xff)),
            other => panic!("{other:?}"),
        }
        // A square is centred: columns 44..84 of 128.
        let rows = to_rows(&[[255, 255, 255, 255]; 4], 2, 2);
        let lit = |x: usize| rows[20 * 16 + x / 8] & (0x80 >> (x % 8)) != 0;
        assert!(!lit(43) && lit(44) && lit(83) && !lit(84));
    }

    #[test]
    fn ggctl_screen_gif_reads_the_file() {
        let path = std::env::temp_dir().join(format!("ggctl-{}-stripes.gif", std::process::id()));
        std::fs::write(&path, STRIPES).unwrap();
        let sent = sends(&format!("screen gif {}", path.display()));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            sent,
            Request::SetScreen {
                screen: Screen::Animation { ref frames, .. }
            } if frames.len() == 3
        ));
        let Err(not_gif) = parse(&args("screen gif /nonexistent/file.gif")) else {
            panic!("a missing file is refused")
        };
        assert!(format!("{not_gif:#}").contains("reading"), "{not_gif:#}");
        assert!(from_gif(b"\x89PNG\r\n\x1a\n").is_err());
        assert!(from_gif(b"GIF89a\x01\x00\x01\x00\x00\x00\x00\x3b").is_err(), "no pictures");
    }
}
