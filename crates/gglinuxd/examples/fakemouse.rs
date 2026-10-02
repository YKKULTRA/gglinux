//! A stand-in for `gglinuxd` that speaks the real protocol against no hardware.
//!
//! ```text
//! fakemouse                       # three devices, battery drifting down from 87%
//! fakemouse --battery 12          # start low, to see the panel's low-charge state
//! fakemouse --charging
//! fakemouse --fail set_mouse_dpi  # make one method return an error
//! fakemouse --no-mouse            # keyboard + headset only
//! ```
//!
//! Two things this buys that the real daemon cannot:
//!
//! - **It writes nothing.** The Rival 3 protocol has no getters, so applying a
//!   setting destroys the record of what preceded it. Every UI path that ends in a
//!   write can be exercised here instead, as often as you like.
//! - **It logs what the UI actually sent.** Since it deserializes with the same
//!   `gg_ipc` types the daemon uses, a frame this accepts is a frame the daemon
//!   accepts, and a malformed one fails here in exactly the same way.
//!
//! It deliberately does *not* validate values the way the device layer does — the
//! point is to observe what the UI sends, including anything out of range.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{
    AppStream, AudioDevice, BandKind, BatteryState, Channel, ChannelEq, ChannelState, DeviceInfo,
    DeviceKind, EqBand, Event, HeadphoneEq, HeadsetChange, MicSettings, MicState, HeadsetControls, HeadsetPower, HeadsetState, InterfaceInfo, KeyboardState, Lighting, MouseDpi, MouseSettings, ReactiveStatus, GameInfo, GameSenseState, ProfilesState, ClipInfo, ClipsState,
    MouseState, Request, RequestFrame, ResponseData, ScreenSettings, ServerMessage, SmartVolume, SmartVolumeState,
    SonarState, StreamMix,
};

/// How often to push a `mouse_battery` notification.
const BATTERY_TICK: Duration = Duration::from_secs(5);

struct Fake {
    battery: Option<BatteryState>,
    applied: Option<MouseSettings>,
    /// Screen settings as last sent, echoed back unvalidated.
    screen: ScreenSettings,
    /// Headset settings as last sent, echoed back as the device would report them.
    controls: HeadsetControls,
    /// Sonar as last changed by the client.
    sonar: SonarState,
    /// Keyboard lighting as last set; no layout, since nothing draws it here.
    keyboard: KeyboardState,
    /// GameSense as the client last set it.
    gamesense: GameSenseState,
    /// Profiles and rules as the client last set them.
    profiles: ProfilesState,
    /// The replay recorder, pretending gpu-screen-recorder is installed.
    clips: ClipsState,
    devices: Vec<DeviceInfo>,
    /// Method name that should return an error instead of succeeding.
    fail: Option<String>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let level: u8 = value("--battery").map_or(Ok(87), |v| v.parse()).context("--battery")?;
    let charging = flag("--charging");
    let with_mouse = !flag("--no-mouse");

    let path = gg_ipc::socket_path();
    // Never shadow a real daemon: if anything answers on this path, a client that
    // connects next would get whichever of us won the race.
    if UnixStream::connect(&path).is_ok() {
        bail!(
            "something is already listening on {} — stop gglinuxd first",
            path.display()
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding {}", path.display()))?;

    let state = Arc::new(Mutex::new(Fake {
        battery: Some(BatteryState { level, charging }),
        applied: None,
        screen: ScreenSettings::default(),
        controls: HeadsetControls::default(),
        sonar: fake_sonar(),
        keyboard: KeyboardState { connected: true, layout: Vec::new(), lighting: Lighting::default(), reactive: ReactiveStatus::Off },
        gamesense: GameSenseState::default(),
        profiles: ProfilesState::default(),
        clips: ClipsState {
            available: true,
            dir: "/home/you/Videos/gglinux".into(),
            clips: vec![ClipInfo { name: "Replay_2026-09-29_20-00-00.mp4".into(), bytes: 48_000_000, modified: 1_790_700_000 }],
            ..ClipsState::default()
        },
        devices: devices(with_mouse),
        fail: value("--fail"),
    }));

    eprintln!("fakemouse listening on {}", path.display());
    eprintln!("  battery {level}%{}", if charging { ", charging" } else { "" });
    if let Some(f) = &state.lock().unwrap().fail {
        eprintln!("  {f} will return an error");
    }
    eprintln!("  waiting for a client (start the UI, or run ggctl)\n");

    for stream in listener.incoming() {
        let stream = stream?;
        let state = Arc::clone(&state);
        thread::spawn(move || {
            if let Err(e) = serve(stream, state) {
                eprintln!("client gone: {e:#}");
            }
        });
    }
    Ok(())
}

fn serve(stream: UnixStream, state: Arc<Mutex<Fake>>) -> Result<()> {
    eprintln!("-- client connected");
    // One lock around the write half: the battery ticker and the request handler
    // both write here, and a half-written line would desync the client's parser.
    let writer = Arc::new(Mutex::new(stream.try_clone()?));

    {
        let devices = state.lock().unwrap().devices.clone();
        send(&writer, ServerMessage::notify(Event::DevicesChanged { devices }))?;
    }

    // Drift the battery so the push path is visible without touching anything.
    let ticker_state = Arc::clone(&state);
    let ticker_writer = Arc::clone(&writer);
    thread::spawn(move || loop {
        thread::sleep(BATTERY_TICK);
        let battery = {
            let mut s = ticker_state.lock().unwrap();
            match &mut s.battery {
                Some(b) if b.charging => b.level = (b.level + 1).min(100),
                Some(b) => b.level = b.level.saturating_sub(1).max(1),
                None => {}
            }
            s.battery
        };
        let msg = ServerMessage::notify(Event::MouseBattery { battery });
        if send(&ticker_writer, msg).is_err() {
            return; // client hung up
        }
    });

    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let frame: RequestFrame = match serde_json::from_str(&line) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("<< UNPARSEABLE {line}\n   {e}");
                send(&writer, ServerMessage::response(0, error(format!("invalid request: {e}"))))?;
                continue;
            }
        };
        eprintln!("<< {}", describe(&frame.req));
        let result = handle(frame.req, &state);
        if let ResponseData::Error { message } = &result {
            eprintln!(">>   error: {message}");
        }
        send(&writer, ServerMessage::response(frame.id, result))?;
    }
    eprintln!("-- client disconnected");
    Ok(())
}

fn handle(req: Request, state: &Arc<Mutex<Fake>>) -> ResponseData {
    let mut s = state.lock().unwrap();

    // `fail` matches on the wire name, so it can target a method by the same
    // string the UI sends.
    if s.fail.as_deref() == Some(method_name(&req)) {
        return error(format!("simulated failure for {}", method_name(&req)));
    }

    match req {
        Request::ListDevices => ResponseData::Devices { devices: s.devices.clone() },
        Request::Ping => ResponseData::Pong,
        Request::GetMouseState => ResponseData::Mouse { state: mouse_state(&s) },
        Request::SetMouseDpi { stages, active, save } => {
            s.applied.get_or_insert_with(MouseSettings::default).dpi = Some(MouseDpi { stages, active });
            note_save(save);
            ResponseData::Success
        }
        Request::SetMousePolling { hz, save } => {
            s.applied.get_or_insert_with(MouseSettings::default).polling_hz = Some(hz);
            note_save(save);
            ResponseData::Success
        }
        Request::SetMouseButtons { actions, save } => {
            s.applied.get_or_insert_with(MouseSettings::default).buttons = Some(actions);
            note_save(save);
            ResponseData::Success
        }
        Request::SaveMouse => {
            eprintln!(">>   (would commit to onboard memory)");
            ResponseData::Success
        }
        Request::GetHeadsetState => ResponseData::Headset { state: headset_state(&s) },
        Request::SetHeadsetScreen { settings } => {
            s.screen = settings;
            ResponseData::Success
        }
        Request::SetHeadset { change } => {
            fake_apply(&mut s.controls, change);
            ResponseData::Success
        }
        Request::GetSonarState => ResponseData::Sonar { state: s.sonar.clone() },
        Request::SetSonarEnabled { enabled } => {
            s.sonar.enabled = enabled;
            ResponseData::Success
        }
        Request::SetSonarOutput { name } => {
            s.sonar.output = Some(name);
            ResponseData::Success
        }
        Request::SetChannelVolume { channel, volume } => {
            if let Some(c) = s.sonar.channels.iter_mut().find(|c| c.channel == channel) {
                c.volume = Some(volume);
            }
            ResponseData::Success
        }
        Request::SetChatMix { balance } => {
            s.sonar.chatmix = balance;
            ResponseData::Success
        }
        Request::SetAppChannel { binary, channel } => {
            for app in s.sonar.apps.iter_mut().filter(|a| a.binary.as_deref() == Some(binary.as_str())) {
                app.channel = channel;
            }
            ResponseData::Success
        }
        Request::SetChannelEq { channel, bands } => {
            if let Some(c) = s.sonar.eq.iter_mut().find(|c| c.channel == channel) {
                c.bands = bands;
            }
            ResponseData::Success
        }
        Request::SetHeadphoneEq { eq } => {
            s.sonar.headphone = eq;
            ResponseData::Success
        }
        Request::ImportAutoEq { name, .. } => {
            s.sonar.headphone.name = name;
            ResponseData::Success
        }
        Request::SetMicInput { name } => {
            s.sonar.mic.input = Some(name);
            ResponseData::Success
        }
        Request::SetMicSettings { settings } => {
            s.sonar.mic.settings = settings;
            ResponseData::Success
        }
        Request::SetMicVolume { volume } => {
            s.sonar.mic.volume = Some(volume);
            ResponseData::Success
        }
        Request::SetStreamer { enabled } => {
            s.sonar.stream.enabled = enabled;
            s.sonar.stream.present = enabled;
            ResponseData::Success
        }
        Request::SetStreamVolume { channel, volume } => {
            if let Some(i) = Channel::ALL.iter().position(|&c| c == channel) {
                s.sonar.stream.volumes[i] = volume;
            }
            ResponseData::Success
        }
        Request::SetStreamMaster { volume } => {
            s.sonar.stream.master = volume;
            ResponseData::Success
        }
        Request::SetSmartVolume { channel, settings } => {
            if let Some(i) = Channel::ALL.iter().position(|&c| c == channel) {
                s.sonar.smart_volume.settings[i] = settings;
            }
            ResponseData::Success
        }
        Request::GetKeyboardState => ResponseData::Keyboard { state: s.keyboard.clone() },
        Request::SetLighting { lighting } => {
            // Like a machine without the optional udev rule, so the UI's hint shows.
            s.keyboard.reactive = if lighting.enabled && lighting.reactive.enabled {
                ReactiveStatus::NoAccess
            } else {
                ReactiveStatus::Off
            };
            s.keyboard.lighting = lighting;
            ResponseData::Success
        }
        Request::GetGamesenseState => ResponseData::Gamesense { state: s.gamesense.clone() },
        Request::SetGamesenseEnabled { enabled } => {
            s.gamesense.enabled = enabled;
            s.gamesense.address = enabled.then(|| "127.0.0.1:51234".to_string());
            s.gamesense.prefixes = if enabled { 17 } else { 0 };
            ResponseData::Success
        }
        Request::SetGameEnabled { game, enabled } => match s.gamesense.games.iter_mut().find(|g| g.name == game) {
            Some(g) => {
                g.enabled = enabled;
                ResponseData::Success
            }
            None => ResponseData::Error { message: format!("unknown game {game}") },
        },
        Request::RemoveGame { game } => {
            s.gamesense.games.retain(|g| g.name != game);
            ResponseData::Success
        }
        Request::GetProfiles => ResponseData::Profiles { state: s.profiles.clone() },
        Request::SaveProfile { name } => {
            if !s.profiles.profiles.contains(&name) {
                s.profiles.profiles.push(name);
            }
            ResponseData::Success
        }
        Request::ApplyProfile { name } => {
            if s.profiles.profiles.contains(&name) {
                ResponseData::Success
            } else {
                ResponseData::Error { message: format!("no profile named \"{name}\"") }
            }
        }
        Request::DeleteProfile { name } => {
            s.profiles.profiles.retain(|p| p != &name);
            ResponseData::Success
        }
        Request::SetGameRules { rules } => {
            s.profiles.rules = rules;
            ResponseData::Success
        }
        Request::GetClipsState => ResponseData::Clips { state: s.clips.clone() },
        Request::SetClipsSettings { settings } => {
            s.clips.enabled = settings.enabled;
            s.clips.recording = settings.enabled && !settings.games_only;
            s.clips.waiting = settings.enabled && settings.games_only;
            s.clips.settings = settings;
            ResponseData::Success
        }
        Request::SaveClip => {
            if s.clips.enabled {
                let n = s.clips.clips.len() + 1;
                let modified = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                s.clips.clips.insert(0, ClipInfo { name: format!("Replay_{n}.mp4"), bytes: 1_000_000, modified });
                ResponseData::Success
            } else {
                ResponseData::Error { message: "clips are off".into() }
            }
        }
        Request::DeleteClip { name } => {
            s.clips.clips.retain(|c| c.name != name);
            ResponseData::Success
        }
        Request::GamesenseTest => {
            if !s.gamesense.games.iter().any(|g| g.name == "GGLINUX_TEST") {
                s.gamesense.games.push(GameInfo {
                    name: "GGLINUX_TEST".into(),
                    display_name: Some("gglinux test".into()),
                    developer: Some("gglinux".into()),
                    enabled: true,
                    active: true,
                    events: vec!["HEALTH".into()],
                });
            }
            ResponseData::Success
        }
    }
}

fn note_save(save: bool) {
    if save {
        eprintln!(">>   (would also commit to onboard memory)");
    }
}

fn mouse_state(s: &Fake) -> MouseState {
    MouseState {
        connected: s.devices.iter().any(|d| d.kind == DeviceKind::Mouse),
        battery: s.battery,
        applied: s.applied.clone(),
    }
}

fn headset_state(s: &Fake) -> HeadsetState {
    HeadsetState {
        connected: s.devices.iter().any(|d| d.kind == DeviceKind::Headset),
        power: HeadsetPower::Online,
        headset_battery: Some(75),
        spare_battery: Some(100),
        screen: s.screen,
        screen_dark: false,
        controls: s.controls.clone(),
    }
}

/// A plausible Sonar: on, two outputs, two apps.
fn fake_sonar() -> SonarState {
    SonarState {
        available: true,
        enabled: true,
        output: Some("alsa_output.fake-headset".into()),
        outputs: vec![
            AudioDevice { name: "alsa_output.fake-headset".into(), description: "Fake Headset".into() },
            AudioDevice { name: "alsa_output.fake-speakers".into(), description: "Fake Speakers".into() },
        ],
        channels: Channel::ALL
            .iter()
            .map(|&channel| ChannelState { channel, present: true, volume: Some(1.0) })
            .collect(),
        chatmix: 0.0,
        apps: vec![
            AppStream { id: 90, name: "Discord".into(), binary: Some("discord".into()), channel: Channel::Chat },
            AppStream { id: 91, name: "Firefox".into(), binary: Some("firefox".into()), channel: Channel::Media },
        ],
        eq: Channel::ALL.iter().map(|&channel| ChannelEq { channel, bands: fake_bands(10) }).collect(),
        headphone: HeadphoneEq { preamp: 0.0, bands: fake_bands(12), name: None },
        mic: MicState {
            available: true,
            present: true,
            input: Some("alsa_input.fake-headset".into()),
            inputs: vec![AudioDevice { name: "alsa_input.fake-headset".into(), description: "Fake Headset Mic".into() }],
            volume: Some(1.0),
            settings: MicSettings::default(),
        },
        stream: StreamMix { enabled: false, present: false, volumes: vec![1.0; 4], master: 1.0 },
        smart_volume: SmartVolumeState { available: true, reason: None, settings: vec![SmartVolume::default(); 4] },
    }
}

/// Flat bands shaped like the daemon's: a low shelf, peaks, a high shelf.
fn fake_bands(n: usize) -> Vec<EqBand> {
    (0..n)
        .map(|i| EqBand {
            kind: if i == 0 {
                BandKind::LowShelf
            } else if i == n - 1 {
                BandKind::HighShelf
            } else {
                BandKind::Peaking
            },
            freq: 31.25 * 2f32.powi(i as i32),
            q: 1.0,
            gain: 0.0,
        })
        .collect()
}

/// Echo a change into the fake's controls, as the real device would report it.
fn fake_apply(c: &mut HeadsetControls, change: HeadsetChange) {
    match change {
        HeadsetChange::NoiseMode(m) => c.noise_mode = Some(m),
        HeadsetChange::Transparency(v) => c.transparency = Some(v),
        HeadsetChange::Sidetone(v) => c.sidetone = Some(v),
        HeadsetChange::MicVolume(v) => c.mic_volume = Some(v),
        HeadsetChange::MicLed(v) => c.mic_led = Some(v),
        HeadsetChange::AutoOffMinutes(v) => c.auto_off_minutes = Some(v),
        HeadsetChange::RadioMode(m) => c.radio_mode = Some(m),
        HeadsetChange::EqPreset(p) => c.eq_preset = Some(p),
        HeadsetChange::EqBands(b) => {
            c.eq_bands = Some(b);
            c.eq_preset = Some(4);
        }
    }
}

fn error(message: String) -> ResponseData {
    ResponseData::Error { message }
}

fn method_name(req: &Request) -> &'static str {
    match req {
        Request::ListDevices => "list_devices",
        Request::Ping => "ping",
        Request::GetMouseState => "get_mouse_state",
        Request::SetMouseDpi { .. } => "set_mouse_dpi",
        Request::SetMousePolling { .. } => "set_mouse_polling",
        Request::SetMouseButtons { .. } => "set_mouse_buttons",
        Request::SaveMouse => "save_mouse",
        Request::GetHeadsetState => "get_headset_state",
        Request::SetHeadsetScreen { .. } => "set_headset_screen",
        Request::SetHeadset { .. } => "set_headset",
        Request::GetSonarState => "get_sonar_state",
        Request::SetSonarEnabled { .. } => "set_sonar_enabled",
        Request::SetSonarOutput { .. } => "set_sonar_output",
        Request::SetChannelVolume { .. } => "set_channel_volume",
        Request::SetChatMix { .. } => "set_chat_mix",
        Request::SetAppChannel { .. } => "set_app_channel",
        Request::SetChannelEq { .. } => "set_channel_eq",
        Request::SetHeadphoneEq { .. } => "set_headphone_eq",
        Request::ImportAutoEq { .. } => "import_auto_eq",
        Request::SetMicInput { .. } => "set_mic_input",
        Request::SetMicSettings { .. } => "set_mic_settings",
        Request::SetMicVolume { .. } => "set_mic_volume",
        Request::SetStreamer { .. } => "set_streamer",
        Request::SetStreamVolume { .. } => "set_stream_volume",
        Request::SetStreamMaster { .. } => "set_stream_master",
        Request::SetSmartVolume { .. } => "set_smart_volume",
        Request::GetKeyboardState => "get_keyboard_state",
        Request::SetLighting { .. } => "set_lighting",
        Request::GetGamesenseState => "get_gamesense_state",
        Request::SetGamesenseEnabled { .. } => "set_gamesense_enabled",
        Request::SetGameEnabled { .. } => "set_game_enabled",
        Request::RemoveGame { .. } => "remove_game",
        Request::GamesenseTest => "gamesense_test",
        Request::GetProfiles => "get_profiles",
        Request::SaveProfile { .. } => "save_profile",
        Request::ApplyProfile { .. } => "apply_profile",
        Request::DeleteProfile { .. } => "delete_profile",
        Request::SetGameRules { .. } => "set_game_rules",
        Request::GetClipsState => "get_clips_state",
        Request::SetClipsSettings { .. } => "set_clips_settings",
        Request::SaveClip => "save_clip",
        Request::DeleteClip { .. } => "delete_clip",
    }
}

/// One-line rendering of a request, so the log shows values and not just methods.
fn describe(req: &Request) -> String {
    match req {
        Request::SetMouseDpi { stages, active, save } => {
            let list: Vec<String> = stages
                .iter()
                .map(|s| if s.x == s.y { s.x.to_string() } else { format!("{}:{}", s.x, s.y) })
                .collect();
            format!("set_mouse_dpi {} active={active} save={save}", list.join(","))
        }
        Request::SetMousePolling { hz, save } => format!("set_mouse_polling {hz}Hz save={save}"),
        Request::SetMouseButtons { actions, save } => {
            format!("set_mouse_buttons {} save={save}", actions.join(","))
        }
        Request::SetHeadsetScreen { settings } => format!(
            "set_headset_screen protect={} lit={}s brightness={:?}",
            settings.protect, settings.lit_seconds, settings.brightness
        ),
        Request::SetHeadset { change } => format!("set_headset {change:?}"),
        other => method_name(other).to_string(),
    }
}

fn send(writer: &Arc<Mutex<UnixStream>>, msg: ServerMessage) -> Result<()> {
    let line = msg.to_line()?;
    let mut w = writer.lock().unwrap();
    w.write_all(line.as_bytes())?;
    w.flush()?;
    Ok(())
}

/// The three real devices, with the interface layout this machine reports, so the
/// UI's device cards look like the ones it will see in production.
fn devices(with_mouse: bool) -> Vec<DeviceInfo> {
    let iface = |n: i32, page: u16, usage: u16, path: &str| InterfaceInfo {
        interface_number: n,
        usage_page: page,
        usage,
        path: path.to_string(),
    };
    let mut out = vec![
        DeviceInfo {
            id: "1038:12e0".into(),
            kind: DeviceKind::Headset,
            vendor_id: 0x1038,
            product_id: 0x12e0,
            name: "Arctis Nova Pro Wireless".into(),
            serial: None,
            connected: true,
            interfaces: vec![
                iface(3, 12, 1, "/dev/hidraw13"),
                iface(4, 0xffc0, 1, "/dev/hidraw14"),
            ],
        },
        DeviceInfo {
            id: "1038:1642".into(),
            kind: DeviceKind::Keyboard,
            vendor_id: 0x1038,
            product_id: 0x1642,
            name: "Apex Pro TKL Gen 3".into(),
            serial: None,
            connected: true,
            interfaces: vec![
                iface(0, 1, 6, "/dev/hidraw4"),
                iface(1, 0xffc0, 1, "/dev/hidraw5"),
                iface(4, 0xffc1, 1, "/dev/hidraw8"),
            ],
        },
    ];
    if with_mouse {
        out.push(DeviceInfo {
            id: "1038:1872".into(),
            kind: DeviceKind::Mouse,
            vendor_id: 0x1038,
            product_id: 0x1872,
            name: "Rival 3 Wireless Gen 2".into(),
            serial: None,
            connected: true,
            interfaces: vec![
                iface(0, 1, 2, "/dev/hidraw0"),
                iface(2, 0xff00, 1, "/dev/hidraw2"),
                iface(3, 0xffc0, 1, "/dev/hidraw3"),
            ],
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}
