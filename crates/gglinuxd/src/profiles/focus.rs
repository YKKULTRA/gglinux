// SPDX-License-Identifier: GPL-3.0-or-later
//! Which window has the focus, for rules that act on focus.
//!
//! Two desktops can say: Hyprland, on its event socket (`activewindowv2>>ADDR`, the address
//! then looked up in its `j/clients` answer for the class and pid), and KDE Plasma, through
//! gglinux's KWin script (`kwin/gglinux-focus`), which calls [`KwinReceiver`] on the session
//! bus each time a window is activated. Where neither does, rules that act on focus act while
//! their program runs instead (the controller's fallback). Each source sends
//! [`FocusEvent`]s; the controller waits [`DEBOUNCE`] after the last one before it switches, so
//! Alt-Tabbing past a window doesn't apply its profile.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// How long the focus stays on a window before its profile goes on (gglinux's own choice).
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// The focused window, as a desktop reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub class: String,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusEvent {
    /// A desktop reports the focused window from now on: its name, for the panel.
    Up(&'static str),
    /// The focus moved; `None` when no window has it.
    Window(Option<Window>),
    /// The source stopped, and why.
    Down(String),
}

/// A desktop that can report the focused window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Hyprland's folder of sockets, `$XDG_RUNTIME_DIR/hypr/<signature>`.
    Hyprland(PathBuf),
    /// The session bus, for the KWin script to call.
    Kwin,
}

/// Where the focused window can be learnt now: Hyprland if its sockets are there, else the
/// session bus if there is one. `runtime` is `$XDG_RUNTIME_DIR`.
pub fn find(runtime: Option<&Path>, signature: Option<&str>, bus: bool) -> Option<Source> {
    if let Some(dir) = runtime.and_then(|r| hyprland_dir(r, signature)) {
        return Some(Source::Hyprland(dir));
    }
    bus.then_some(Source::Kwin)
}

/// The Hyprland instance's folder: the one `signature` names, else the newest that has an
/// event socket (a daemon started by systemd often doesn't have Hyprland's variables).
pub fn hyprland_dir(runtime: &Path, signature: Option<&str>) -> Option<PathBuf> {
    let hypr = runtime.join("hypr");
    if let Some(sig) = signature.filter(|s| !s.is_empty() && !s.contains('/')) {
        let dir = hypr.join(sig);
        if dir.join(".socket2.sock").exists() {
            return Some(dir);
        }
    }
    std::fs::read_dir(&hypr)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|d| d.join(".socket2.sock").exists())
        .max_by_key(|d| {
            std::fs::metadata(d.join(".socket2.sock"))
                .and_then(|m| m.modified())
                .ok()
        })
}

/// Whether a session bus can be reached without guessing `/run/user/<uid>/bus`.
pub fn session_bus(runtime: Option<&Path>) -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|a| !a.is_empty())
        || runtime.is_some_and(|r| r.join("bus").exists())
}

/// One line of Hyprland's event socket that matters here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyprEvent {
    /// `activewindow>>CLASS,TITLE`: the class, for when the address can't be looked up.
    Class(String),
    /// `activewindowv2>>ADDR`, without `0x`; empty when no window has the focus.
    Address(String),
}

pub fn parse_line(line: &str) -> Option<HyprEvent> {
    let (name, data) = line.trim_end().split_once(">>")?;
    match name {
        "activewindow" => Some(HyprEvent::Class(
            data.split_once(',').map_or(data, |(c, _)| c).to_string(),
        )),
        "activewindowv2" => Some(HyprEvent::Address(
            data.trim_start_matches("0x").trim_matches(',').to_string(),
        )),
        _ => None,
    }
}

fn window_of(v: &serde_json::Value) -> Option<Window> {
    let class = v.get("class")?.as_str()?.to_string();
    let pid = v
        .get("pid")
        .and_then(|p| p.as_i64())
        .and_then(|p| u32::try_from(p).ok())
        .filter(|&p| p > 0);
    Some(Window { class, pid })
}

/// The window at `address` (without `0x`) in Hyprland's `j/clients` answer.
pub fn client(clients: &str, address: &str) -> Option<Window> {
    let list: serde_json::Value = serde_json::from_str(clients).ok()?;
    list.as_array()?
        .iter()
        .find(|c| {
            c.get("address")
                .and_then(|a| a.as_str())
                .is_some_and(|a| a.trim_start_matches("0x").eq_ignore_ascii_case(address))
        })
        .and_then(window_of)
}

/// The window in Hyprland's `j/activewindow` answer (`{}` when none has the focus).
pub fn active(answer: &str) -> Option<Window> {
    let v: serde_json::Value = serde_json::from_str(answer).ok()?;
    window_of(&v)
}

/// Ask Hyprland's request socket, as `hyprctl` does: the request, then its whole answer.
async fn hypr_ask(dir: &Path, what: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(dir.join(".socket.sock")).await?;
    s.write_all(what.as_bytes()).await?;
    let mut answer = String::new();
    tokio::time::timeout(Duration::from_secs(2), s.read_to_string(&mut answer))
        .await
        .map_err(|_| std::io::Error::other("Hyprland didn't answer"))??;
    Ok(answer)
}

/// Follow Hyprland's focus until its event socket closes.
pub async fn hyprland(dir: PathBuf, tx: mpsc::UnboundedSender<FocusEvent>) {
    let events = match UnixStream::connect(dir.join(".socket2.sock")).await {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(FocusEvent::Down(format!("Hyprland's event socket: {e}")));
            return;
        }
    };
    let _ = tx.send(FocusEvent::Up("Hyprland"));
    // The window that has the focus already.
    if let Ok(answer) = hypr_ask(&dir, "j/activewindow").await {
        let _ = tx.send(FocusEvent::Window(active(&answer)));
    }
    let mut lines = BufReader::new(events).lines();
    let mut class = None;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match parse_line(&line) {
                Some(HyprEvent::Class(c)) => class = Some(c),
                Some(HyprEvent::Address(a)) if a.is_empty() => {
                    let _ = tx.send(FocusEvent::Window(None));
                }
                Some(HyprEvent::Address(a)) => {
                    let found = match hypr_ask(&dir, "j/clients").await {
                        Ok(clients) => client(&clients, &a),
                        Err(_) => None,
                    };
                    let w = found.or_else(|| {
                        class.clone().map(|class| Window { class, pid: None })
                    });
                    let _ = tx.send(FocusEvent::Window(w));
                }
                None => {}
            },
            Ok(None) => break,
            Err(e) => {
                let _ = tx.send(FocusEvent::Down(format!("Hyprland's event socket: {e}")));
                return;
            }
        }
    }
    let _ = tx.send(FocusEvent::Down("Hyprland closed its event socket".into()));
}

/// The KWin script's bus name, object and interface (`kwin/gglinux-focus/contents/code/main.js`
/// names the same).
pub const KWIN_NAME: &str = "io.github.ykk.gglinux.Focus";
pub const KWIN_PATH: &str = "/io/github/ykk/gglinux/Focus";
/// Named again in [`KwinReceiver`]'s `zbus::interface` attribute, which takes no constant.
#[cfg(test)]
const KWIN_INTERFACE: &str = "io.github.ykk.gglinux.Focus";

/// What the KWin script calls.
pub struct KwinReceiver {
    tx: mpsc::UnboundedSender<FocusEvent>,
    /// Whether the script has called yet: until then Plasma isn't reporting anything.
    heard: bool,
}

impl KwinReceiver {
    pub fn new(tx: mpsc::UnboundedSender<FocusEvent>) -> KwinReceiver {
        KwinReceiver { tx, heard: false }
    }
}

#[zbus::interface(name = "io.github.ykk.gglinux.Focus")]
impl KwinReceiver {
    /// A window was activated: its `resourceClass` and pid; an empty class when none has the
    /// focus.
    fn window_activated(&mut self, class: String, pid: i32) {
        if !self.heard {
            self.heard = true;
            let _ = self.tx.send(FocusEvent::Up("KDE Plasma"));
        }
        let window = (!class.is_empty()).then(|| Window {
            class,
            pid: u32::try_from(pid).ok().filter(|&p| p > 0),
        });
        let _ = self.tx.send(FocusEvent::Window(window));
    }
}

/// Take the bus name the KWin script calls. The connection serves it while it's kept.
pub async fn kwin(tx: mpsc::UnboundedSender<FocusEvent>) -> Result<zbus::Connection, String> {
    let built = async {
        zbus::connection::Builder::session()?
            .serve_at(KWIN_PATH, KwinReceiver::new(tx))?
            .name(KWIN_NAME)?
            .build()
            .await
    };
    match tokio::time::timeout(Duration::from_secs(5), built).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => Err(format!("the session bus: {e}")),
        Err(_) => Err("the session bus didn't answer".into()),
    }
}

/// When a change of focus has held long enough to act on.
#[derive(Debug, Default)]
pub struct Debounce {
    deadline: Option<Instant>,
}

impl Debounce {
    /// The focus moved at `now`: wait again from there.
    pub fn moved(&mut self, now: Instant) {
        self.deadline = Some(now + DEBOUNCE);
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Whether to act at `now`; once it says so, it waits for the next move.
    pub fn due(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(d) if now >= d => {
                self.deadline = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
pub mod testing {
    //! A Hyprland made of sockets in a scratch folder, for tests. Never the real one.

    use std::path::Path;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    /// Hyprland's `j/clients` answer: a game and a terminal.
    pub const CLIENTS: &str = r#"[
        {"address": "0x55d0c3a5e8b0", "class": "steam_app_1174180", "title": "Red Dead Redemption 2", "pid": 4242},
        {"address": "0x55d0c3a60000", "class": "kitty", "title": "fish", "pid": 1777}
    ]"#;

    /// An event socket that sends each line after its pause (ms), then closes, or with
    /// `hold` stays open; and a request socket answering `j/activewindow` (the terminal) and
    /// `j/clients`.
    pub async fn fake_hyprland(dir: &Path, lines: Vec<(u64, &'static str)>, hold: bool) {
        let events = UnixListener::bind(dir.join(".socket2.sock")).unwrap();
        let requests = UnixListener::bind(dir.join(".socket.sock")).unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = requests.accept().await else { return };
                let mut asked = vec![0u8; 64];
                let n = s.read(&mut asked).await.unwrap_or(0);
                let answer = match &asked[..n] {
                    b"j/activewindow" => r#"{"address":"0x55d0c3a60000","class":"kitty","pid":1777}"#,
                    b"j/clients" => CLIENTS,
                    _ => "unknown request",
                };
                let _ = s.write_all(answer.as_bytes()).await;
            }
        });
        tokio::spawn(async move {
            let (mut s, _) = events.accept().await.unwrap();
            for (pause, line) in lines {
                tokio::time::sleep(Duration::from_millis(pause)).await;
                if s.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                    return;
                }
            }
            if hold {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scratch folder of one test, removed when it ends.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("gg-focus-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hyprland_lines_give_the_class_and_the_address() {
        assert_eq!(
            parse_line("activewindow>>kitty,~/src: fish"),
            Some(HyprEvent::Class("kitty".into()))
        );
        assert_eq!(
            parse_line("activewindowv2>>55d0c3a5e8b0\n"),
            Some(HyprEvent::Address("55d0c3a5e8b0".into()))
        );
        assert_eq!(parse_line("activewindowv2>>"), Some(HyprEvent::Address(String::new())));
        assert_eq!(parse_line("activewindowv2>>,"), Some(HyprEvent::Address(String::new())));
        assert_eq!(parse_line("workspace>>2"), None);
        assert_eq!(parse_line("garbage"), None);
    }

    #[test]
    fn a_window_is_found_by_its_address_in_hyprlands_clients() {
        assert_eq!(
            client(CLIENTS, "55d0c3a5e8b0"),
            Some(Window {
                class: "steam_app_1174180".into(),
                pid: Some(4242)
            })
        );
        assert_eq!(client(CLIENTS, "dead"), None);
        assert_eq!(client("not json", "55d0c3a5e8b0"), None);
        assert_eq!(
            active(r#"{"address":"0x1","class":"kitty","pid":1777}"#),
            Some(Window {
                class: "kitty".into(),
                pid: Some(1777)
            })
        );
        assert_eq!(active("{}"), None, "no window has the focus");
    }

    #[test]
    fn the_hyprland_folder_is_the_named_one_else_the_newest() {
        let d = Dir::new("dirs");
        assert_eq!(hyprland_dir(&d.0, None), None);
        for sig in ["old", "new"] {
            std::fs::create_dir_all(d.0.join("hypr").join(sig)).unwrap();
            std::fs::write(d.0.join("hypr").join(sig).join(".socket2.sock"), "").unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::create_dir_all(d.0.join("hypr/empty")).unwrap();
        assert_eq!(hyprland_dir(&d.0, None), Some(d.0.join("hypr/new")));
        assert_eq!(hyprland_dir(&d.0, Some("old")), Some(d.0.join("hypr/old")));
        assert_eq!(hyprland_dir(&d.0, Some("gone")), Some(d.0.join("hypr/new")));
        assert_eq!(
            find(Some(&d.0), None, true),
            Some(Source::Hyprland(d.0.join("hypr/new")))
        );
        assert_eq!(find(None, None, true), Some(Source::Kwin));
        assert_eq!(find(None, None, false), None);
    }

    use testing::{fake_hyprland, CLIENTS};

    #[tokio::test]
    async fn hyprland_reports_the_window_it_starts_on_then_each_switch() {
        let d = Dir::new("hypr");
        let lines = "workspace>>1\nactivewindow>>steam_app_1174180,Red Dead Redemption 2\nactivewindowv2>>55d0c3a5e8b0\nactivewindow>>,\nactivewindowv2>>\nactivewindow>>gone,Gone\nactivewindowv2>>dead";
        let lines: Vec<(u64, &'static str)> = lines.lines().map(|l| (5, l)).collect();
        fake_hyprland(&d.0, lines, false).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        hyprland(d.0.clone(), tx).await;
        let mut got = Vec::new();
        while let Ok(e) = rx.try_recv() {
            got.push(e);
        }
        let kitty = Window {
            class: "kitty".into(),
            pid: Some(1777),
        };
        let game = Window {
            class: "steam_app_1174180".into(),
            pid: Some(4242),
        };
        assert_eq!(
            got,
            [
                FocusEvent::Up("Hyprland"),
                FocusEvent::Window(Some(kitty)),
                FocusEvent::Window(Some(game)),
                FocusEvent::Window(None),
                // Not in the clients: the class from the line before.
                FocusEvent::Window(Some(Window {
                    class: "gone".into(),
                    pid: None
                })),
                FocusEvent::Down("Hyprland closed its event socket".into()),
            ]
        );
    }

    #[tokio::test]
    async fn no_hyprland_socket_is_a_source_down() {
        let d = Dir::new("nohypr");
        let (tx, mut rx) = mpsc::unbounded_channel();
        hyprland(d.0.clone(), tx).await;
        assert!(matches!(rx.try_recv(), Ok(FocusEvent::Down(why)) if why.contains("event socket")));
    }

    #[test]
    fn the_kwin_script_reports_each_window_and_says_plasma_is_up_once() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut r = KwinReceiver::new(tx);
        r.window_activated("steam_app_1174180".into(), 4242);
        r.window_activated(String::new(), 0);
        r.window_activated("kitty".into(), -1);
        let mut got = Vec::new();
        while let Ok(e) = rx.try_recv() {
            got.push(e);
        }
        assert_eq!(
            got,
            [
                FocusEvent::Up("KDE Plasma"),
                FocusEvent::Window(Some(Window {
                    class: "steam_app_1174180".into(),
                    pid: Some(4242)
                })),
                FocusEvent::Window(None),
                FocusEvent::Window(Some(Window {
                    class: "kitty".into(),
                    pid: None
                })),
            ]
        );
    }

    /// The script gglinux ships for KWin, and the package around it.
    const SCRIPT: &str = include_str!("kwin/gglinux-focus/contents/code/main.js");
    const METADATA: &str = include_str!("kwin/gglinux-focus/metadata.json");

    #[test]
    fn the_kwin_script_calls_what_the_daemon_serves() {
        for name in [KWIN_NAME, KWIN_PATH, KWIN_INTERFACE, "WindowActivated", "callDBus"] {
            assert!(SCRIPT.contains(name), "{name}");
        }
        let meta: serde_json::Value = serde_json::from_str(METADATA).unwrap();
        assert_eq!(meta["KPackageStructure"], "KWin/Script");
        assert_eq!(meta["X-Plasma-API"], "javascript");
        assert_eq!(meta["X-Plasma-MainScript"], "code/main.js");
        assert_eq!(meta["KPlugin"]["Id"], "gglinux-focus");
    }

    #[test]
    fn a_change_of_focus_waits_until_it_has_held() {
        let t = Instant::now();
        let mut d = Debounce::default();
        assert!(!d.due(t));
        d.moved(t);
        assert!(!d.due(t + DEBOUNCE / 2));
        d.moved(t + DEBOUNCE / 2); // Alt-Tab goes past a window
        assert!(!d.due(t + DEBOUNCE));
        assert!(d.due(t + DEBOUNCE / 2 + DEBOUNCE));
        assert!(!d.due(t + DEBOUNCE * 3), "once");
        assert_eq!(d.deadline(), None);
    }
}
