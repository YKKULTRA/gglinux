// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl screen`: what the keyboard's screen shows, and its lines in `ggctl keyboard`.

use anyhow::{bail, Context, Result};
use gg_ipc::{KeyboardState, Request, Screen};

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
        _ => bail!("usage: ggctl screen keyboard | clock [seconds] [12h] [date] | text ... | image file.pbm"),
    };
    screen.validate().map_err(anyhow::Error::msg)?;
    Ok(Request::SetScreen { screen })
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

/// The screen and screen care lines of `ggctl keyboard`.
pub fn print(state: &KeyboardState) {
    println!(
        "screen:    {}",
        match &state.screen {
            Screen::Keyboard => "the keyboard's own".to_owned(),
            Screen::Text { text } => format!("text {:?}", text),
            Screen::Clock { .. } => "clock".to_owned(),
            Screen::Image { .. } => "picture".to_owned(),
            Screen::Animation { frames, .. } => format!("animation of {} pictures", frames.len()),
            Screen::Monitor { items } => format!("system monitor {items:?}"),
        }
    );
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
}
