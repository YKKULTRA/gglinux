// SPDX-License-Identifier: GPL-3.0-or-later
//! CLI client for gglinuxd. Installed as `ggctl`; from a checkout:
//! `cargo run -p gglinuxd --bin ggctl`. The commands are in [`USAGE`], which
//! `ggctl --help` prints.
//!
//! Prefer this over `gg-device-mouse`'s `mousectl` whenever the daemon is running:
//! the daemon holds the device, and two writers on one hidraw node interleave badly.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{
    DpiStage, KeyboardState, MouseDpi, Request, RequestFrame, ResponseData, Screen, ServerMessage,
    Switches,
};

/// What `ggctl --help` prints.
const USAGE: &str = "\
ggctl                                   # watch: device list, then live events
ggctl mouse                             # battery + settings we have applied
ggctl mouse dpi 800,1600 [--select N] [--save]   # N counts from 0
ggctl mouse dpi 400:800,1600:1600       # per-axis X:Y
ggctl mouse polling 1000 [--save]
ggctl mouse buttons button1,button2,button3,button4,button5,dpi [--save]
ggctl mouse save                        # commit whatever is applied to onboard memory
ggctl clip save                         # save the instant replay (bind this to a key)
ggctl profile list                      # saved lighting profiles
ggctl profile apply <name>
ggctl gamesense test|on|off
ggctl keyboard                          # switch settings and screen, as the daemon has them
ggctl switches on|off                   # gglinux's switch settings, or leave them alone
ggctl switches actuation 1.5            # every key's actuation point, 0.1 to 4.0 mm
ggctl switches rapid-trigger on [0.2]|off   # optional re-trigger distance, 0.1 to 2.0 mm
ggctl screen keyboard                   # hand the screen back to the keyboard's own menus
ggctl screen clock [seconds] [12h] [date]
ggctl screen text Hello|world           # up to 5 lines of 16 characters; | starts a line
ggctl screen image picture.pbm          # a 128x40 PBM (P1 or P4)
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Before connecting, so a typo fails the same whether or not the daemon runs.
    let request = match parse(&args)? {
        Action::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        Action::Watch => return watch(connect()?),
        Action::Send(request) => request,
        Action::EditSwitches(edit) => {
            let ResponseData::Keyboard { state } = send(connect()?, Request::GetKeyboardState)?
            else {
                bail!("the daemon did not send the keyboard state");
            };
            let mut switches = state.switches;
            edit.apply(&mut switches);
            Request::SetSwitches { switches }
        }
    };

    match send(connect()?, request)? {
        ResponseData::Mouse { state } => {
            println!(
                "connected: {}\nbattery:   {}",
                state.connected,
                match state.battery {
                    Some(b) => format!(
                        "{}%{}",
                        b.level,
                        if b.charging { " (charging)" } else { "" }
                    ),
                    None => "unknown (asleep — move the mouse and retry)".into(),
                }
            );
            match state.applied {
                Some(s) => {
                    if let Some(dpi) = &s.dpi {
                        println!("dpi:       {}", dpi_summary(dpi));
                    }
                    if let Some(buttons) = &s.buttons {
                        println!("buttons:   {}", buttons.join(","));
                    }
                    if let Some(hz) = s.polling_hz {
                        println!("polling:   {hz} Hz");
                    }
                }
                // The mouse protocol is write-only, so an unwritten setting is
                // genuinely unknown rather than assumed to be the default.
                None => println!(
                    "applied:   nothing yet this session (mouse is on its onboard settings)"
                ),
            }
        }
        ResponseData::Keyboard { state } => print_keyboard(&state),
        ResponseData::Profiles { state } => {
            for name in state.profiles {
                println!("{name}");
            }
        }
        ResponseData::Success => {
            println!("ok");
            // Only the mouse forgets what isn't saved; `mouse save` is the saving itself.
            let save = args.iter().any(|a| a == "--save");
            if !save
                && matches!(positional(&args)?.as_slice(), ["mouse", command, ..] if *command != "save")
            {
                println!("note: not saved — pass --save to keep this across a power cycle");
            }
            if matches!(positional(&args)?.as_slice(), ["switches", "off"]) {
                println!(
                    "note: the keyboard keeps the switch settings it has until it's unplugged"
                );
            }
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

/// What the command line asks for.
enum Action {
    /// Stream everything the daemon sends until it goes quiet.
    Watch,
    Send(Request),
    /// Print the commands.
    Help,
    /// Read the switch settings, change them, and send them back.
    EditSwitches(SwitchEdit),
}

/// One change to the switch settings.
#[derive(Debug, PartialEq)]
enum SwitchEdit {
    Enable(bool),
    /// Every key's actuation point, in tenths of a millimetre; per-key points are dropped.
    Actuation(u8),
    RapidTrigger {
        on: bool,
        sensitivity: Option<u8>,
    },
}

impl SwitchEdit {
    fn apply(self, s: &mut Switches) {
        match self {
            SwitchEdit::Enable(on) => s.enabled = on,
            SwitchEdit::Actuation(tenths) => {
                s.enabled = true;
                s.actuation = tenths;
                s.keys.clear();
            }
            SwitchEdit::RapidTrigger { on, sensitivity } => {
                s.enabled |= on;
                s.rapid_trigger.enabled = on;
                if let Some(d) = sensitivity {
                    s.rapid_trigger.sensitivity = d;
                }
            }
        }
    }
}

/// `1.5` (millimetres) as tenths of a millimetre.
fn tenths(mm: &str) -> Result<u8> {
    let v: f32 = mm
        .parse()
        .with_context(|| format!("'{mm}' is not a distance in mm"))?;
    let t = (v * 10.0).round();
    if !(1.0..=40.0).contains(&t) {
        bail!("{mm} mm is out of range (0.1 to 4.0)");
    }
    Ok(t as u8)
}

fn switches_action(rest: &[&str]) -> Result<Action> {
    Ok(match rest {
        [] => Action::Send(Request::GetKeyboardState),
        ["on"] => Action::EditSwitches(SwitchEdit::Enable(true)),
        ["off"] => Action::EditSwitches(SwitchEdit::Enable(false)),
        ["actuation", mm] => Action::EditSwitches(SwitchEdit::Actuation(tenths(mm)?)),
        ["rapid-trigger", "on"] => Action::EditSwitches(SwitchEdit::RapidTrigger {
            on: true,
            sensitivity: None,
        }),
        ["rapid-trigger", "on", mm] => {
            let d = tenths(mm)?;
            if d > 20 {
                bail!("the Rapid Trigger distance goes up to 2.0 mm");
            }
            Action::EditSwitches(SwitchEdit::RapidTrigger {
                on: true,
                sensitivity: Some(d),
            })
        }
        ["rapid-trigger", "off"] => Action::EditSwitches(SwitchEdit::RapidTrigger {
            on: false,
            sensitivity: None,
        }),
        _ => bail!("usage: ggctl switches on|off | actuation 1.5 | rapid-trigger on [0.2]|off"),
    })
}

fn screen_request(rest: &[&str]) -> Result<Request> {
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

fn print_keyboard(state: &KeyboardState) {
    let mm = |t: u8| format!("{:.1} mm", f32::from(t) / 10.0);
    println!("connected: {}", if state.connected { "yes" } else { "no" });
    let s = &state.switches;
    if s.enabled {
        let own = if s.keys.is_empty() {
            String::new()
        } else {
            format!(" ({} keys with their own)", s.keys.len())
        };
        let rt = &s.rapid_trigger;
        let rapid = if rt.enabled {
            let keys = if rt.keys.is_empty() {
                "every key".to_owned()
            } else {
                format!("{} keys", rt.keys.len())
            };
            format!("on, {}, {keys}", mm(rt.sensitivity))
        } else {
            "off".to_owned()
        };
        let tap = if s.rapid_tap.enabled {
            format!("on, {} pairs", s.rapid_tap.pairs.len())
        } else {
            "off".to_owned()
        };
        println!(
            "switches:  actuation {}{own}; Rapid Trigger {rapid}; Rapid Tap {tap}",
            mm(s.actuation)
        );
    } else {
        println!("switches:  left to the keyboard");
    }
    if let Some(e) = &state.switches_error {
        println!("           the keyboard refused them: {e}");
    }
    println!(
        "screen:    {}",
        match &state.screen {
            Screen::Keyboard => "the keyboard's own".to_owned(),
            Screen::Text { text } => format!("text {:?}", text),
            Screen::Clock { .. } => "clock".to_owned(),
            Screen::Image { .. } => "picture".to_owned(),
        }
    );
    if let Some(e) = &state.screen_error {
        println!("           the keyboard refused it: {e}");
    }
}

fn parse(args: &[String]) -> Result<Action> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Action::Help);
    }
    let save = args.iter().any(|a| a == "--save");
    Ok(Action::Send(match positional(args)?.split_first() {
        None => return Ok(Action::Watch),
        Some((&"mouse", rest)) => mouse_request(rest, args, save)?,
        Some((&"clip", ["save"])) => Request::SaveClip,
        Some((&"clip", _)) => bail!("usage: ggctl clip save"),
        Some((&"profile", ["list"])) => Request::GetProfiles,
        // Names may have spaces, so `profile apply Late Night` needs no quotes.
        Some((&"profile", ["apply", name @ ..])) if !name.is_empty() => Request::ApplyProfile {
            name: name.join(" "),
        },
        Some((&"profile", _)) => bail!("usage: ggctl profile list | ggctl profile apply <name>"),
        Some((&"gamesense", ["test"])) => Request::GamesenseTest,
        Some((&"gamesense", ["on"])) => Request::SetGamesenseEnabled { enabled: true },
        Some((&"gamesense", ["off"])) => Request::SetGamesenseEnabled { enabled: false },
        Some((&"gamesense", _)) => bail!("usage: ggctl gamesense test|on|off"),
        Some((&"keyboard", [])) => Request::GetKeyboardState,
        Some((&"switches", rest)) => return switches_action(rest),
        Some((&"screen", rest)) => screen_request(rest)?,
        Some((other, _)) => {
            bail!("unknown command '{other}' (try: mouse, clip, profile, gamesense, keyboard, switches, screen, or nothing to watch)")
        }
    }))
}

/// The arguments that aren't `--flags`, or the value that follows `--select`.
///
/// A flag we don't know is an error rather than skipped, since the mouse can't be
/// read back: a typo like `--sava` would otherwise write something unsaved.
fn positional(args: &[String]) -> Result<Vec<&str>> {
    let mut words = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(a) = args.next() {
        match a {
            // Its number, which would otherwise be taken for the DPI list.
            "--select" => {
                args.next().context("--select expects a number")?;
            }
            "--save" => {}
            a if a.starts_with("--select=") => {}
            a if a.starts_with("--") => {
                bail!("unknown option '{a}' (try: --save, --select N, --help)")
            }
            a => words.push(a),
        }
    }
    Ok(words)
}

/// Each mouse command takes exactly one value, so `dpi 800 1600` (a space where
/// the comma belongs) is refused rather than sent as the single stage 800.
fn mouse_request(rest: &[&str], args: &[String], save: bool) -> Result<Request> {
    Ok(match rest.split_first() {
        None => Request::GetMouseState,
        Some((&"dpi", [spec])) => Request::SetMouseDpi {
            stages: parse_stages(spec)?,
            active: flag_value(args, "--select")
                .map(|v| v.parse::<usize>().context("--select expects a number"))
                .transpose()?
                .unwrap_or(0),
            save,
        },
        Some((&"dpi", _)) => bail!("usage: ggctl mouse dpi 800,1600 [--select N] [--save]"),
        Some((&"polling", [hz])) => Request::SetMousePolling {
            hz: hz.parse().context("polling rate must be a number")?,
            save,
        },
        Some((&"polling", _)) => bail!("usage: ggctl mouse polling 1000 [--save]"),
        Some((&"buttons", [actions])) => Request::SetMouseButtons {
            actions: actions
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            save,
        },
        Some((&"buttons", _)) => bail!("usage: ggctl mouse buttons button1,...,dpi [--save]"),
        Some((&"save", [])) => Request::SaveMouse,
        Some((&"save", _)) => bail!("usage: ggctl mouse save"),
        Some((other, _)) => {
            bail!("unknown mouse command '{other}' (try: dpi, polling, buttons, save)")
        }
    })
}

fn connect() -> Result<UnixStream> {
    let path = gg_ipc::socket_path();
    UnixStream::connect(&path)
        .with_context(|| format!("connecting to {} — is gglinuxd running?", path.display()))
}

/// Send one request and return its response, ignoring events that arrive first.
fn send(mut stream: UnixStream, req: Request) -> Result<ResponseData> {
    const ID: u64 = 1;
    let frame = serde_json::to_string(&RequestFrame { id: ID, req })?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    writeln!(stream, "{frame}")?;

    for line in BufReader::new(stream).lines() {
        let line = line.context("daemon closed the connection or timed out")?;
        match serde_json::from_str::<ServerMessage>(&line) {
            // The daemon pushes a snapshot on connect; that is not our answer.
            Ok(ServerMessage::Notification { .. }) => continue,
            Ok(ServerMessage::Response { result, .. }) => {
                if let ResponseData::Error { message } = result {
                    bail!("{message}");
                }
                return Ok(result);
            }
            Err(e) => bail!("could not parse daemon reply: {e}\n  {line}"),
        }
    }
    bail!("no response from daemon")
}

/// Print every message until the daemon goes quiet for two seconds.
fn watch(stream: UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut stream = stream;
    writeln!(stream, r#"{{"id":1,"req":{{"method":"list_devices"}}}}"#)?;

    for line in BufReader::new(stream).lines() {
        match line {
            Ok(l) => println!("{l}"),
            Err(_) => break, // read timeout — nothing more to read
        }
    }
    Ok(())
}

/// The value given as `--flag N` or `--flag=N`.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .enumerate()
        .find_map(|(i, a)| match a.strip_prefix(flag)? {
            "" => args.get(i + 1).map(String::as_str),
            rest => rest.strip_prefix('='),
        })
}

/// The applied DPI stages, and which one is active.
///
/// The stage is numbered from 0, as `--select` takes it, so the number shown here
/// can be passed straight back.
fn dpi_summary(dpi: &MouseDpi) -> String {
    let stages: Vec<String> = dpi
        .stages
        .iter()
        .map(|d| {
            if d.x == d.y {
                d.x.to_string()
            } else {
                format!("{}:{}", d.x, d.y)
            }
        })
        .collect();
    format!(
        "{} (stage {} active, counting from 0)",
        stages.join(","),
        dpi.active
    )
}

/// Parse `800,1600` or `400:800,1600:1600` into DPI stages.
fn parse_stages(spec: &str) -> Result<Vec<DpiStage>> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|part| {
            Ok(match part.split_once(':') {
                Some((x, y)) => DpiStage {
                    x: x.trim().parse().context("invalid X DPI")?,
                    y: y.trim().parse().context("invalid Y DPI")?,
                },
                None => {
                    let dpi = part.parse().context("invalid DPI")?;
                    DpiStage { x: dpi, y: dpi }
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn sends(line: &str) -> Request {
        match parse(&args(line)).unwrap() {
            Action::Send(r) => r,
            Action::Watch => panic!("{line} parsed as watch"),
            Action::Help => panic!("{line} parsed as help"),
            Action::EditSwitches(e) => panic!("{line} parsed as a switch edit {e:?}"),
        }
    }

    #[test]
    fn no_arguments_watches() {
        assert!(matches!(parse(&[]).unwrap(), Action::Watch));
    }

    #[test]
    fn the_new_commands_map_to_their_requests() {
        assert!(matches!(sends("clip save"), Request::SaveClip));
        assert!(matches!(sends("profile list"), Request::GetProfiles));
        assert!(matches!(sends("gamesense test"), Request::GamesenseTest));
        assert!(matches!(
            sends("gamesense on"),
            Request::SetGamesenseEnabled { enabled: true }
        ));
        assert!(matches!(
            sends("gamesense off"),
            Request::SetGamesenseEnabled { enabled: false }
        ));
        assert!(matches!(sends("mouse"), Request::GetMouseState));
        assert!(matches!(
            sends("mouse polling 1000"),
            Request::SetMousePolling {
                hz: 1000,
                save: false
            }
        ));
    }

    #[test]
    fn profile_names_may_contain_spaces() {
        match sends("profile apply Late Night") {
            Request::ApplyProfile { name } => assert_eq!(name, "Late Night"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_select_value_is_not_read_as_the_dpi_list() {
        for line in [
            "mouse dpi --select 1 800,1600 --save",
            "mouse dpi 800,1600 --select 1 --save",
        ] {
            match sends(line) {
                Request::SetMouseDpi {
                    stages,
                    active,
                    save,
                } => {
                    assert_eq!(
                        stages,
                        [DpiStage { x: 800, y: 800 }, DpiStage { x: 1600, y: 1600 }],
                        "{line}"
                    );
                    assert_eq!((active, save), (1, true), "{line}");
                }
                other => panic!("{line}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_active_stage_shows_as_the_number_select_takes() {
        let applied = MouseDpi {
            stages: [400, 800, 1600].map(|d| DpiStage { x: d, y: d }).to_vec(),
            active: 1,
        };
        let shown = dpi_summary(&applied);
        let n = shown
            .split("stage ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .unwrap();
        // Passing back the number `ggctl mouse` shows keeps that stage active.
        match sends(&format!("mouse dpi 400,800,1600 --select {n}")) {
            Request::SetMouseDpi { active, .. } => assert_eq!(active, applied.active, "{shown}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mistakes_are_errors() {
        for bad in [
            "clip",
            "clip delete",
            "profile",
            "profile apply",
            "gamesense maybe",
            "frobnicate",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_mouse_command_with_a_stray_word_or_flag_writes_nothing() {
        for bad in [
            // A space where the comma belongs would otherwise send just the first stage.
            "mouse dpi 800 1600 --save",
            "mouse dpi",
            "mouse dpi 800,1600 --sava",
            "mouse dpi 800,1600 --select",
            "mouse polling 1000 500",
            "mouse buttons button1, button2",
            "mouse save now",
            "--version",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn select_takes_the_equals_form_too() {
        match sends("mouse dpi 400,800 --select=1") {
            Request::SetMouseDpi { active, .. } => assert_eq!(active, 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn help_is_asked_for_rather_than_watching() {
        for line in ["--help", "-h", "mouse dpi 800,1600 --save --help"] {
            assert!(matches!(parse(&args(line)), Ok(Action::Help)), "{line}");
        }
    }

    fn edits(line: &str) -> SwitchEdit {
        match parse(&args(line)).unwrap() {
            Action::EditSwitches(e) => e,
            _ => panic!("{line} is not a switch edit"),
        }
    }

    #[test]
    fn switch_commands_edit_what_the_daemon_has() {
        assert_eq!(edits("switches on"), SwitchEdit::Enable(true));
        assert_eq!(edits("switches actuation 1.5"), SwitchEdit::Actuation(15));
        assert_eq!(
            edits("switches rapid-trigger on 0.2"),
            SwitchEdit::RapidTrigger {
                on: true,
                sensitivity: Some(2)
            }
        );
        assert!(parse(&args("switches actuation 4.5")).is_err());
        assert!(parse(&args("switches rapid-trigger on 2.5")).is_err());
        assert!(matches!(sends("switches"), Request::GetKeyboardState));

        let mut s = Switches {
            keys: vec![gg_ipc::KeyActuation {
                code: 0x04,
                actuation: 3,
            }],
            ..Switches::default()
        };
        SwitchEdit::Actuation(20).apply(&mut s);
        assert!(s.enabled && s.actuation == 20 && s.keys.is_empty());
        SwitchEdit::RapidTrigger {
            on: true,
            sensitivity: Some(5),
        }
        .apply(&mut s);
        assert!(s.rapid_trigger.enabled && s.rapid_trigger.sensitivity == 5);
    }

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
