// SPDX-License-Identifier: GPL-3.0-or-later
//! The client against a daemon of the test's own: a socket it answers by hand.

use super::*;
use std::io::{BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream as StdStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Instant;

use gg_ipc::DeviceKind;

const WAIT: Duration = Duration::from_secs(5);

/// A daemon socket in a folder of its own.
struct Fake {
    dir: PathBuf,
    path: PathBuf,
    listener: UnixListener,
}

impl Fake {
    fn new() -> Fake {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gglinux-client-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Fake {
            dir,
            path,
            listener,
        }
    }

    /// The client's next connection.
    fn accept(&self) -> Peer {
        let until = Instant::now() + WAIT;
        loop {
            match self.listener.accept() {
                Ok((s, _)) => {
                    s.set_nonblocking(false).unwrap();
                    s.set_read_timeout(Some(WAIT)).unwrap();
                    return Peer {
                        read: std::io::BufReader::new(s.try_clone().unwrap()),
                        write: s,
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("the client didn't connect: {e}"),
            }
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The daemon's end of one connection.
struct Peer {
    read: std::io::BufReader<StdStream>,
    write: StdStream,
}

impl Peer {
    fn request(&mut self) -> RequestFrame {
        let mut line = String::new();
        self.read.read_line(&mut line).expect("a request");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"))
    }

    /// Write several messages at once, as one read on the other side.
    fn send(&mut self, messages: &[ServerMessage]) {
        let text: String = messages.iter().map(|m| m.to_line().unwrap()).collect();
        self.write.write_all(text.as_bytes()).unwrap();
    }

    fn answer(&mut self, id: u64, result: ResponseData) {
        self.send(&[ServerMessage::response(id, result)]);
    }

    /// Answer the reads a connection starts with: `parts` (only the clips run),
    /// no devices, `clips` for the recorder, and the states outside the parts.
    fn warm_up(&mut self, clips: &str) {
        let r = self.request();
        assert!(matches!(r.req, Request::GetParts), "{:?}", r.req);
        self.answer(
            r.id,
            ResponseData::Parts {
                parts: clips_only(),
            },
        );
        for _ in 0..4 {
            let r = self.request();
            match r.req {
                Request::ListDevices => {
                    self.answer(r.id, ResponseData::Devices { devices: vec![] })
                }
                Request::GetClipsState => self.answer(r.id, clips_answer(clips)),
                Request::GetExperimental => self.answer(
                    r.id,
                    ResponseData::Experimental {
                        experimental: gg_ipc::Experimental::default(),
                    },
                ),
                Request::GetActions => self.answer(
                    r.id,
                    ResponseData::Actions {
                        state: gg_ipc::ActionsState::default(),
                    },
                ),
                other => panic!("a read it shouldn't make: {other:?}"),
            }
        }
    }
}

fn clips_only() -> Parts {
    Parts {
        keyboard: false,
        headset: false,
        mouse: false,
        sonar: false,
        gamesense: false,
        profiles: false,
        clips: true,
    }
}

fn clips(dir: &str) -> ClipsState {
    ClipsState {
        dir: dir.into(),
        ..ClipsState::default()
    }
}

fn clips_answer(dir: &str) -> ResponseData {
    ResponseData::Clips { state: clips(dir) }
}

type Changes = std_mpsc::Receiver<(Changed, State)>;

fn client(fake: &Fake) -> (Client, Changes) {
    client_at(fake.path.clone())
}

fn client_at(path: PathBuf) -> (Client, Changes) {
    let (tx, rx) = std_mpsc::channel();
    let tx = Mutex::new(tx);
    let client = Client::start_at(path, move |what, s| {
        let _ = tx.lock().unwrap().send((what, s));
    });
    (client, rx)
}

/// The state at the first change `f` accepts.
fn until(changes: &Changes, f: impl Fn(Changed, &State) -> bool) -> State {
    let until = Instant::now() + WAIT;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let (what, s) = changes.recv_timeout(left).expect("the change");
        if f(what, &s) {
            return s;
        }
    }
}

fn ask(
    client: &Client,
    req: Request,
    timeout: Duration,
) -> std_mpsc::Receiver<Result<ResponseData, String>> {
    let (tx, rx) = std_mpsc::channel();
    client.request(req, timeout, move |r| {
        let _ = tx.send(r);
    });
    rx
}

/// Connected, and the first reads answered.
fn warmed(fake: &Fake) -> (Client, Changes, Peer) {
    let (c, changes) = client(fake);
    let mut peer = fake.accept();
    until(&changes, |w, s| w == Changed::Connection && s.connected);
    peer.warm_up("first");
    until(&changes, |_, s| s.clips.is_some() && s.parts.is_some());
    (c, changes, peer)
}

#[test]
fn answers_and_notifications_land_in_the_order_they_came() {
    let fake = Fake::new();
    let (c, changes, mut peer) = warmed(&fake);
    // As after a change: the part's state is read again.
    c.refresh_part(Part::Clips);
    let r = peer.request();
    assert!(matches!(r.req, Request::GetClipsState));
    // The answer, and right behind it a notification of a newer state.
    peer.send(&[
        ServerMessage::response(r.id, clips_answer("older")),
        ServerMessage::notify(Event::ClipsChanged {
            state: clips("newer"),
        }),
    ]);
    until(&changes, |w, s| {
        w == Changed::Clips && s.clips.as_ref().is_some_and(|c| c.dir == "newer")
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(c.state().clips.unwrap().dir, "newer");
}

#[test]
fn a_request_waiting_when_the_daemon_leaves_fails_at_once() {
    let fake = Fake::new();
    let (c, changes, mut peer) = warmed(&fake);
    let rx = ask(&c, Request::GetMouseState, Duration::from_secs(30));
    peer.request();
    let asked = Instant::now();
    drop(peer);
    assert_eq!(rx.recv_timeout(WAIT).unwrap().unwrap_err(), GONE);
    assert!(asked.elapsed() < Duration::from_secs(2));

    // What that daemon said goes with it, but the parts.
    let s = until(&changes, |w, s| w == Changed::Connection && !s.connected);
    assert!(s.clips.is_none() && s.devices.is_empty());
    assert_eq!(s.parts, Some(clips_only()));

    // While it's away, a request fails without waiting.
    let rx = ask(&c, Request::Ping, Duration::from_secs(30));
    assert_eq!(rx.recv_timeout(WAIT).unwrap().unwrap_err(), AWAY);

    // It comes back, and everything is read again.
    let mut peer = fake.accept();
    peer.warm_up("again");
    let s = until(&changes, |_, s| {
        s.clips.as_ref().is_some_and(|c| c.dir == "again")
    });
    assert!(s.connected);
}

#[test]
fn a_request_nobody_answers_times_out_and_its_late_answer_is_harmless() {
    let fake = Fake::new();
    let (c, changes, mut peer) = warmed(&fake);
    let rx = ask(&c, Request::GetClipsState, Duration::from_millis(200));
    let late = peer.request();
    let err = rx.recv_timeout(WAIT).unwrap().unwrap_err();
    assert!(err.contains("didn't answer"), "{err}");

    // It's still the daemon's latest word, so it's kept.
    peer.answer(late.id, clips_answer("late"));
    until(&changes, |_, s| {
        s.clips.as_ref().is_some_and(|c| c.dir == "late")
    });

    let rx = ask(&c, Request::Ping, WAIT);
    let r = peer.request();
    assert_ne!(r.id, late.id);
    peer.answer(r.id, ResponseData::Pong);
    assert!(matches!(
        rx.recv_timeout(WAIT).unwrap(),
        Ok(ResponseData::Pong)
    ));
}

#[test]
fn an_answer_this_build_cannot_read_fails_its_request_at_once() {
    let fake = Fake::new();
    let (c, _changes, mut peer) = warmed(&fake);
    let rx = ask(&c, Request::Ping, Duration::from_secs(30));
    let r = peer.request();
    let line = format!(
        "{{\"kind\":\"response\",\"id\":{},\"result\":{{\"type\":\"from_the_future\"}}}}\n",
        r.id
    );
    peer.write.write_all(line.as_bytes()).unwrap();
    let err = rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert!(err.contains("can't read"), "{err}");
}

#[test]
fn a_change_of_parts_waits_for_the_restart() {
    let fake = Fake::new();
    let (c, changes, mut peer) = warmed(&fake);
    let (tx, done) = std_mpsc::channel();
    c.set_parts(Parts::default(), move |r| {
        let _ = tx.send(r);
    });
    let r = peer.request();
    assert!(matches!(r.req, Request::SetParts { .. }));
    peer.answer(r.id, ResponseData::Success);
    assert!(done.recv_timeout(WAIT).unwrap().is_ok());
    until(&changes, |w, s| w == Changed::Parts && s.restarting);

    // The daemon starts again: the old parts stay meanwhile, so no page flashes.
    drop(peer);
    let s = until(&changes, |w, s| w == Changed::Connection && !s.connected);
    assert!(s.restarting);
    assert_eq!(s.parts, Some(clips_only()));

    let mut peer = fake.accept();
    let r = peer.request();
    assert!(matches!(r.req, Request::GetParts));
    peer.answer(
        r.id,
        ResponseData::Parts {
            parts: Parts::default(),
        },
    );
    let s = until(&changes, |w, _| w == Changed::Parts);
    assert!(!s.restarting);
    assert_eq!(s.parts, Some(Parts::default()));
}

#[test]
fn an_old_daemon_that_cannot_say_runs_every_part() {
    let fake = Fake::new();
    let (_c, changes) = client(&fake);
    let mut peer = fake.accept();
    let r = peer.request();
    peer.answer(
        r.id,
        ResponseData::Error {
            message: "invalid request: unknown variant `get_parts`".into(),
        },
    );
    let s = until(&changes, |w, _| w == Changed::Parts);
    assert_eq!(s.parts, Some(Parts::default()));
}

#[test]
fn a_daemon_that_leaves_before_saying_leaves_the_parts_unknown() {
    let fake = Fake::new();
    let (_c, changes) = client(&fake);
    let mut peer = fake.accept();
    peer.request();
    drop(peer);
    let s = until(&changes, |w, s| w == Changed::Connection && !s.connected);
    assert_eq!(s.parts, None);
}

#[test]
fn a_restart_that_never_comes_back_stops_being_shown() {
    let fake = Fake::new();
    let path = fake.path.clone();
    drop(fake); // nobody there
    let (c, changes) = client_at(path);
    c.shared.update(|s| {
        s.restarting = true;
        None
    });
    runtime().spawn(c.clone().after_restart(Duration::from_millis(50)));
    let s = until(&changes, |w, _| w == Changed::Parts);
    assert!(!s.restarting && !s.connected);
}

#[test]
fn the_connection_stops_with_the_last_handle() {
    let fake = Fake::new();
    let (c, changes, peer) = warmed(&fake);
    drop(c);
    // The client hangs up rather than waiting on.
    let mut peer = peer;
    let mut line = String::new();
    assert_eq!(peer.read.read_line(&mut line).unwrap(), 0);
    until(&changes, |w, s| w == Changed::Connection && !s.connected);
}

// --- The state on its own -------------------------------------------------------

fn device(id: &str, name: &str) -> DeviceInfo {
    DeviceInfo {
        id: id.into(),
        kind: DeviceKind::Mouse,
        vendor_id: 0x1038,
        product_id: 0x1872,
        name: name.into(),
        serial: None,
        connected: true,
        interfaces: vec![],
    }
}

#[test]
fn a_device_announced_twice_is_listed_once() {
    let mut s = State::default();
    s.apply(Event::DeviceAdded(device("b", "B")));
    s.apply(Event::DeviceAdded(device("a", "A")));
    assert_eq!(
        s.apply(Event::DeviceAdded(device("b", "B again"))),
        Changed::Devices
    );
    let names: Vec<_> = s.devices.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["A", "B again"]);
    s.apply(Event::DeviceRemoved { id: "a".into() });
    assert_eq!(s.devices.len(), 1);
}

#[test]
fn a_battery_alone_is_not_a_mouse_state() {
    let battery = Some(gg_ipc::BatteryState {
        level: 40,
        charging: false,
    });
    let mut s = State::default();
    s.apply(Event::MouseBattery { battery });
    assert!(s.mouse.is_none());
    s.mouse = Some(MouseState {
        connected: true,
        battery: None,
        applied: None,
        ..MouseState::default()
    });
    s.apply(Event::MouseBattery { battery });
    assert_eq!(s.mouse.unwrap().battery, battery);
}

#[test]
fn a_profiles_notification_keeps_the_running_programs() {
    let mut s = State::default();
    s.keep(&ResponseData::Profiles {
        state: ProfilesState {
            running: vec!["game".into()],
            ..ProfilesState::default()
        },
    });
    s.apply(Event::ProfilesChanged {
        state: ProfilesState {
            profiles: vec!["quiet".into()],
            ..ProfilesState::default()
        },
    });
    let p = s.profiles.unwrap();
    assert_eq!(p.profiles, ["quiet"]);
    assert_eq!(p.running, ["game"]);
}

#[test]
fn only_answers_with_a_state_change_it() {
    let mut s = State::default();
    assert_eq!(s.keep(&ResponseData::Success), None);
    assert_eq!(s.keep(&ResponseData::Pong), None);
    assert_eq!(
        s.keep(&ResponseData::Error {
            message: "no".into()
        }),
        None
    );
    s.restarting = true;
    assert_eq!(
        s.keep(&ResponseData::Parts {
            parts: clips_only()
        }),
        Some(Changed::Parts)
    );
    assert!(!s.restarting, "a daemon that answers isn't restarting");
}

#[test]
fn losing_the_daemon_keeps_only_the_parts() {
    let mut s = State {
        connected: true,
        devices: vec![device("a", "A")],
        clips: Some(clips("x")),
        parts: Some(clips_only()),
        restarting: true,
        ..State::default()
    };
    s.lose();
    assert!(!s.connected && s.devices.is_empty() && s.clips.is_none());
    assert_eq!(s.parts, Some(clips_only()));
    assert!(s.restarting);
}

#[test]
fn only_a_missing_daemon_is_a_lost_connection() {
    assert!(lost_connection(GONE) && lost_connection(AWAY));
    assert!(!lost_connection("get_mouse_state: gglinuxd didn't answer"));
    assert!(!lost_connection("no mouse connected"));
}

#[test]
fn a_request_is_named_as_in_the_protocol() {
    assert_eq!(method(&Request::GetMouseState), "get_mouse_state");
    assert_eq!(
        method(&Request::SetSonarEnabled { enabled: true }),
        "set_sonar_enabled"
    );
}

#[test]
fn the_parts_default_to_all_until_read() {
    let s = State::default();
    assert!(Part::ALL.iter().all(|p| s.running().on(*p)));
}
