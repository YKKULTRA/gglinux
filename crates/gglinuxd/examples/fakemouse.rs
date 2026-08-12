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
    BatteryState, DeviceInfo, DeviceKind, DpiStage, Event, InterfaceInfo, MouseSettings, MouseState,
    Request, RequestFrame, ResponseData, ServerMessage,
};

/// How often to push a `mouse_battery` notification.
const BATTERY_TICK: Duration = Duration::from_secs(5);

struct Fake {
    battery: Option<BatteryState>,
    applied: Option<MouseSettings>,
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
            let settings = s.applied.get_or_insert_with(defaults);
            settings.dpi_stages = stages;
            settings.active_stage = active;
            note_save(save);
            ResponseData::Success
        }
        Request::SetMousePolling { hz, save } => {
            s.applied.get_or_insert_with(defaults).polling_hz = hz;
            note_save(save);
            ResponseData::Success
        }
        Request::SetMouseButtons { actions, save } => {
            s.applied.get_or_insert_with(defaults).buttons = actions;
            note_save(save);
            ResponseData::Success
        }
        Request::SaveMouse => {
            eprintln!(">>   (would commit to onboard memory)");
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

/// What the daemon would report before anything has been written to it.
fn defaults() -> MouseSettings {
    MouseSettings {
        dpi_stages: vec![DpiStage { x: 800, y: 800 }, DpiStage { x: 1600, y: 1600 }],
        active_stage: 0,
        polling_hz: 1000,
        buttons: ["button1", "button2", "button3", "button4", "button5", "dpi"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
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
