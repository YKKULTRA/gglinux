// SPDX-License-Identifier: GPL-3.0-or-later
//! CLI client for gglinuxd. Installed as `ggctl`; from a checkout:
//! `cargo run -p gglinuxd --bin ggctl`. The commands are in [`USAGE`], which
//! `ggctl --help` prints.
//!
//! Prefer this over `gg-device-mouse`'s `mousectl` whenever the daemon is running:
//! the daemon holds the device, and two writers on one hidraw node interleave badly.
//!
//! Each command group has its file: [`mouse`], [`keyboard`] (with the switches),
//! [`screen`], [`care`], [`parts`], [`clip`], [`profile`], [`gamesense`] and
//! [`experimental`], [`actions`], [`bind`] and [`onboard`].

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{Feature, Part, Request, RequestFrame, ResponseData, ServerMessage};

mod actions;
mod bind;
mod care;
mod clip;
mod experimental;
mod gamesense;
mod headset;
mod keyboard;
mod macros;
mod mouse;
mod onboard;
mod parts;
mod profile;
mod screen;

/// What `ggctl --help` prints.
const USAGE: &str = "\
ggctl                                   # watch: device list, then live events
ggctl mouse                             # battery + settings we have applied
ggctl mouse dpi 800,1600 [--select N] [--save]   # N counts from 0
ggctl mouse dpi 400:800,1600:1600       # per-axis X:Y
ggctl mouse polling 1000 [--save]
ggctl mouse buttons button1,button2,button3,button4,button5,dpi [--save]
ggctl mouse save                        # commit whatever is applied to onboard memory
ggctl mouse sleep 20                    # sleep timer, 1 to 20 min; saved (needs mouse_power)
ggctl mouse radio normal|stable|efficient [--save]   # Wireless Stability, High-Efficiency Mode
ggctl headset                           # the headset and base station: battery and every setting
ggctl headset anc off|transparency|on   # also: transparency 1-10, sidetone off|low|medium|high,
                                        #   mic-volume 1-10, mic-led 1-10, auto-off never|1|5|10|15|30|60,
                                        #   radio speed|range, eq flat|bass|focus|smiley, eq-custom 0,…,1.5
ggctl headset gain low|high             # the base station's own settings (need headset_extras):
ggctl headset bluetooth-at-power-on on|off   # Bluetooth on when the headset powers on
ggctl headset bluetooth-call nothing|lower|mute   # what a call does to the game sound
ggctl headset line-out speakers|stream  # and stream-mix MAIN,AUX,MIC (0-100 each)
ggctl headset home-screen detailed|simple
ggctl headset screen-dim never|1|5|10|15|30|60   # minutes before the base-station screen dims
ggctl clip save                         # save the instant replay (bind this to a key)
ggctl profile list                      # saved lighting profiles
ggctl profile apply <name>
ggctl gamesense test|on|off
ggctl keyboard                          # switch settings and screen, as the daemon has them
ggctl switches on|off                   # gglinux's switch settings, or leave them alone
ggctl switches actuation 1.5            # every key's actuation point, 0.1 to 4.0 mm
ggctl switches rapid-trigger on [0.2]|off   # optional re-trigger distance, 0.1 to 2.0 mm
ggctl switches protection on|off        # Protection Mode on every key (needs keyboard_protection)
ggctl screen keyboard                   # hand the screen back to the keyboard's own menus
ggctl screen clock [seconds] [12h] [date]
ggctl screen text Hello|world           # up to 5 lines of 16 characters; | starts a line
ggctl screen image picture.pbm          # a 128x40 PBM (P1 or P4)
ggctl screen gif animation.gif          # up to 256 pictures, each for its delay (0.1 to 10 s)
ggctl screen monitor cpu,temp,ram,clock # 1 to 4 of: cpu, temp, ram, gpu, gpu-temp, clock
ggctl screen care shift|away|own on|off # burn-in care: move the picture; dark while away;
                                        #   the keyboard's own screen dark while away too
ggctl parts                             # which parts of gglinux run
ggctl parts sonar off                   # turn a part off or on (keyboard, headset, mouse,
                                        #   sonar, gamesense, profiles, clips); restarts the daemon
ggctl experimental                      # features not tried on your hardware yet: on or off
ggctl experimental keyboard_onboard on  # turn one on or off, by the name listed
ggctl bind                              # key bindings gglinux sends, and the keyboard's as last read
ggctl bind read                         # read the keyboard's bindings back (all three layers)
ggctl bind on|off                       # on: send them each time the keyboard connects; off: send
                                        #   nothing more (what was sent stays until it's unplugged)
ggctl bind capslock ctrl+a              # up to 4 keys; or media:mute, mouse:4, cpi, wheel:up,
                                        #   pan:left, none, default
ggctl bind fn+f1 media:mute             # with Fn held (or --layer fn); deep+w … past W's deep press
ggctl onboard                           # the keyboard's onboard profile slots, 1 to 5
ggctl onboard read [N]                  # read slot N (or all five) and save a backup of each
ggctl onboard load N                    # make the keyboard run slot N
ggctl onboard write N current           # write gglinux's screen picture and Rapid Tap into slot N
ggctl onboard restore N <backup file>   # write a backup into slot N (also: write N <file>)
                                        #   both back the slot up first and put it back on any failure
ggctl onboard restore N <file> --damaged  # put a backup into a slot that doesn't check out (asks first)
ggctl actions                           # actions on the spare keys F13–F24, and what the desktop bound
ggctl actions set f13 run kitty         # or: type <text>, profile <name>, clip, media playpause|next|
                                        #   previous|stop|mute|volup|voldown; a program's own options
                                        #   in quotes: run \"kitty --single-instance\"
ggctl actions rm f13
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
        Action::EditCare(edit) => {
            let ResponseData::Keyboard { state } = send(connect()?, Request::GetKeyboardState)?
            else {
                bail!("the daemon did not send the keyboard state");
            };
            let mut care = state.screen_care;
            edit.apply(&mut care);
            Request::SetScreenCare { care }
        }
        Action::SetPart(part, on) => {
            let ResponseData::Parts { mut parts } = send(connect()?, Request::GetParts)? else {
                bail!("the daemon did not send its parts (is it older than ggctl?)");
            };
            parts.set(part, on);
            Request::SetParts { parts }
        }
        Action::SetFeature(feature, on) => {
            let ResponseData::Experimental { mut experimental } =
                send(connect()?, Request::GetExperimental)?
            else {
                bail!(
                    "the daemon did not send its experimental settings (is it older than ggctl?)"
                );
            };
            experimental.set(feature, on);
            Request::SetExperimental { experimental }
        }
        Action::EditActions(edit) => {
            let read = || match send(connect()?, Request::GetActions)? {
                ResponseData::Actions { state } => Ok(state),
                _ => bail!("the daemon did not send its actions (is it older than ggctl?)"),
            };
            let mut bindings = read()?.bindings;
            edit.apply(&mut bindings)?;
            send(
                connect()?,
                Request::SetActions {
                    bindings: bindings.clone(),
                },
            )?;
            println!("ok");
            if !actions::read_back(&bindings, read)? {
                println!("note: the daemon doesn't show the change yet");
            }
            return Ok(());
        }
    };

    match send(connect()?, request)? {
        ResponseData::Mouse { state } => mouse::print(&state),
        ResponseData::Headset { state } => headset::print(&state),
        ResponseData::Keyboard { state } => keyboard::print(&state),
        ResponseData::Parts { parts } => parts::print(&parts),
        ResponseData::Profiles { state } => profile::print(&state),
        ResponseData::Experimental { experimental } => experimental::print(&experimental),
        ResponseData::Actions { state } => actions::print(&state),
        ResponseData::Bindings { state } => bind::print(&state),
        ResponseData::Onboard { state } => onboard::print(&state),
        ResponseData::Success => {
            println!("ok");
            // Only the mouse forgets what isn't saved; `mouse save` is the saving itself.
            let save = args.iter().any(|a| a == "--save");
            if !save
                && matches!(positional(&args)?.as_slice(), ["mouse", command, ..] if !["save", "sleep"].contains(command))
            {
                println!("note: not saved — pass --save to keep this across a power cycle");
            }
            if matches!(positional(&args)?.as_slice(), ["switches", "off"]) {
                println!(
                    "note: the keyboard keeps the switch settings it has until it's unplugged"
                );
            }
            if matches!(positional(&args)?.as_slice(), ["parts", ..]) {
                println!("the daemon is starting again with that change");
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
    EditSwitches(keyboard::SwitchEdit),
    /// Read the screen care, change one setting, and send it back.
    EditCare(care::CareEdit),
    /// Read the parts, turn one on or off, and send them back.
    SetPart(Part, bool),
    /// Read the experimental gates, turn one on or off, and send them back.
    SetFeature(Feature, bool),
    /// Read the actions, change one, and send them back.
    EditActions(actions::Edit),
}

fn on_off(word: &str) -> Result<bool> {
    match word {
        "on" => Ok(true),
        "off" => Ok(false),
        other => bail!("'{other}' is not on or off"),
    }
}

fn parse(args: &[String]) -> Result<Action> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Action::Help);
    }
    let save = args.iter().any(|a| a == "--save");
    Ok(Action::Send(match positional(args)?.split_first() {
        None => return Ok(Action::Watch),
        Some((&"mouse", rest)) => mouse::request(rest, args, save)?,
        Some((&"clip", rest)) => clip::request(rest)?,
        Some((&"profile", rest)) => profile::request(rest)?,
        Some((&"gamesense", rest)) => gamesense::request(rest)?,
        Some((&"headset", rest)) => headset::request(rest)?,
        Some((&"macro", rest)) => macros::request(rest)?,
        Some((&"keyboard", [])) => Request::GetKeyboardState,
        Some((&"switches", rest)) => return keyboard::switches_action(rest),
        Some((&"screen", ["care", rest @ ..])) => return care::action(rest),
        Some((&"screen", rest)) => screen::request(rest)?,
        Some((&"parts", rest)) => return parts::action(rest),
        Some((&"experimental", rest)) => return experimental::action(rest),
        Some((&"bind", rest)) => return bind::action(rest),
        Some((&"onboard", rest)) => return onboard::action(rest),
        Some((&"actions", rest)) => {
            return Ok(match actions::command(rest)? {
                actions::Command::List => Action::Send(Request::GetActions),
                actions::Command::Edit(edit) => Action::EditActions(edit),
            })
        }
        Some((other, _)) => {
            bail!("unknown command '{other}' (try: mouse, clip, profile, gamesense, keyboard, switches, screen, parts, experimental, or nothing to watch)")
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
            // `ggctl bind`'s layer, which bind.rs reads from the words.
            "--layer" => {
                words.push(a);
                if let Some(v) = args.next() {
                    words.push(v);
                }
            }
            a if a.starts_with("--layer=") => words.push(a),
            // `ggctl onboard restore`'s, which onboard.rs reads from the words.
            "--damaged" => words.push(a),
            a if a.starts_with("--") => {
                bail!("unknown option '{a}' (try: --save, --select N, --help)")
            }
            a => words.push(a),
        }
    }
    Ok(words)
}

fn connect() -> Result<UnixStream> {
    let path = gg_ipc::socket_path();
    UnixStream::connect(&path)
        .with_context(|| format!("connecting to {} — is gglinuxd running?", path.display()))
}

/// How long to wait for the daemon's answer to `req`, and what to say if none comes. A
/// slot read or write can take most of a minute when the keyboard is slow to answer.
fn answer_wait(req: &Request) -> (Duration, &'static str) {
    match req {
        Request::ReadOnboard { .. } | Request::WriteOnboard { .. } => (
            Duration::from_secs(60),
            "no answer from the daemon within a minute: it may still be reading or writing the \
             keyboard's slot. Leave the keyboard plugged in and the daemon running; wait, then run \
             `ggctl onboard` to see how it ended",
        ),
        _ => (
            Duration::from_secs(10),
            "daemon closed the connection or timed out",
        ),
    }
}

/// Send one request and return its response, ignoring events that arrive first.
fn send(mut stream: UnixStream, req: Request) -> Result<ResponseData> {
    const ID: u64 = 1;
    let (wait, silent) = answer_wait(&req);
    let frame = serde_json::to_string(&RequestFrame { id: ID, req })?;
    stream.set_read_timeout(Some(wait))?;
    writeln!(stream, "{frame}")?;

    for line in BufReader::new(stream).lines() {
        let line = line.context(silent)?;
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

/// The command line `line`, as words.
#[cfg(test)]
fn args(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// The request `line` sends straight away.
#[cfg(test)]
fn sends(line: &str) -> Request {
    match parse(&args(line)).unwrap() {
        Action::Send(r) => r,
        Action::Watch => panic!("{line} parsed as watch"),
        Action::Help => panic!("{line} parsed as help"),
        Action::EditSwitches(e) => panic!("{line} parsed as a switch edit {e:?}"),
        Action::EditCare(e) => panic!("{line} parsed as a care edit {e:?}"),
        Action::SetPart(p, on) => panic!("{line} parsed as part {p:?} {on}"),
        Action::SetFeature(f, on) => panic!("{line} parsed as experimental {f:?} {on}"),
        Action::EditActions(e) => panic!("{line} parsed as an actions edit {e:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use care::CareEdit;

    #[test]
    fn screen_care_and_parts_read_their_settings_first() {
        match parse(&args("screen care shift off")).unwrap() {
            Action::EditCare(e) => assert_eq!(e, CareEdit::Shift(false)),
            _ => panic!("not a care edit"),
        }
        match parse(&args("screen care own on")).unwrap() {
            Action::EditCare(e) => assert_eq!(e, CareEdit::OwnScreenToo(true)),
            _ => panic!("not a care edit"),
        }
        assert!(parse(&args("screen care shift maybe")).is_err());
        assert!(matches!(sends("parts"), Request::GetParts));
        match parse(&args("parts Sonar off")).unwrap() {
            Action::SetPart(p, on) => assert_eq!((p, on), (Part::Sonar, false)),
            _ => panic!("not a part change"),
        }
        match parse(&args("parts gamesense on")).unwrap() {
            Action::SetPart(p, on) => assert_eq!((p, on), (Part::Gamesense, true)),
            _ => panic!("not a part change"),
        }
        assert!(parse(&args("parts radio off")).is_err());
        // `screen clock` is still a screen, not care.
        assert!(matches!(sends("screen clock"), Request::SetScreen { .. }));
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
    fn help_is_asked_for_rather_than_watching() {
        for line in ["--help", "-h", "mouse dpi 800,1600 --save --help"] {
            assert!(matches!(parse(&args(line)), Ok(Action::Help)), "{line}");
        }
    }

    #[test]
    fn bind_and_onboard_send_their_requests() {
        assert!(matches!(sends("onboard"), Request::GetOnboard));
        assert!(matches!(sends("bind"), Request::GetBindings));
        assert!(matches!(sends("bind read"), Request::ReadBindings));
    }

    #[test]
    fn actions_list_or_read_change_and_write_back() {
        assert!(matches!(sends("actions"), Request::GetActions));
        assert!(matches!(
            parse(&args("actions set f13 run kitty")).unwrap(),
            Action::EditActions(actions::Edit::Set(_))
        ));
        assert!(matches!(
            parse(&args("actions rm f13")).unwrap(),
            Action::EditActions(actions::Edit::Remove(0x68))
        ));
        assert!(parse(&args("actions set f12 clip")).is_err());
    }

    #[test]
    fn a_slot_read_or_write_is_waited_for_a_minute() {
        for req in [
            Request::ReadOnboard { slot: None },
            Request::WriteOnboard {
                slot: 4,
                from: gg_ipc::OnboardSource::Current,
            },
        ] {
            let (wait, silent) = answer_wait(&req);
            assert_eq!(wait, Duration::from_secs(60));
            assert!(
                silent.contains("may still be") && silent.contains("`ggctl onboard`"),
                "{silent}"
            );
        }
        assert_eq!(answer_wait(&Request::GetOnboard).0, Duration::from_secs(10));
        // `--damaged` reaches `ggctl onboard` as a word.
        assert_eq!(
            positional(&args("onboard restore 5 x.bin --damaged")).unwrap(),
            ["onboard", "restore", "5", "x.bin", "--damaged"]
        );
    }

    #[test]
    fn the_usage_lists_every_command_group() {
        for group in [
            "mouse",
            "clip",
            "profile",
            "gamesense",
            "keyboard",
            "switches",
            "screen",
            "parts",
            "experimental",
            "bind",
            "onboard",
            "actions",
        ] {
            assert!(USAGE.contains(&format!("ggctl {group}")), "{group}");
        }
    }
}
