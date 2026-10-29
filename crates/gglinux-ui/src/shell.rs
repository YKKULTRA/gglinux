// SPDX-License-Identifier: GPL-3.0-or-later
//! What makes it a desktop app: one copy at a time, a tray icon, and a window
//! that closes into the tray.
//!
//! A second launch (the app menu, while the copy started at login sits in the
//! tray) asks the running copy over D-Bus to show its window, and exits: there's
//! only ever one tray icon and one daemon connection.
//!
//! The window only hides when there's a tray to find the app in again. With none
//! (no session bus, or a desktop without a StatusNotifierWatcher), closing the
//! window quits, and a `--hidden` start opens it anyway; if the tray goes away
//! while the window is hidden and doesn't come back, the window opens.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ksni::TrayMethods;
use slint::{CloseRequestResponse, ComponentHandle};

use crate::AppWindow;

/// The app's id: the desktop entry's name, the Wayland app id, the D-Bus name.
pub const APP_ID: &str = "io.github.ykk.gglinux";
const OBJECT: &str = "/io/github/ykk/gglinux";
const INTERFACE: &str = "io.github.ykk.gglinux.App";
/// How long the session bus has to answer: one that hangs mustn't keep the app
/// from starting.
const BUS_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the tray may be missing before the window opens so the app can be
/// found: at login the panel can start after the app, and a panel that restarts
/// is gone for a moment.
const TRAY_GRACE: Duration = Duration::from_secs(10);

/// The D-Bus name this copy answers to. A debug build (`cargo run`) gets one of
/// its own: under the app's, it would hand over to the installed copy sitting in
/// the tray and quit without a word.
pub fn bus_name() -> String {
    if cfg!(debug_assertions) {
        format!("{APP_ID}.dev")
    } else {
        APP_ID.to_string()
    }
}

/// Started at login with `--hidden`: stay in the tray until asked.
pub fn hidden(args: &[String]) -> bool {
    args.iter().any(|a| a == "--hidden")
}

/// Whether the window opens at start: unless started hidden with a tray to wait
/// in. A tray that's running but not shown yet gets [`TRAY_GRACE`] to appear.
pub fn opens_at_start(hidden: bool, has_tray: bool) -> bool {
    !hidden || !has_tray
}

pub fn show_main(ui: &AppWindow) {
    if let Err(e) = ui.show() {
        eprintln!("gglinux: the window didn't open: {e}");
    }
    // Minimised (X11), it comes back up.
    ui.window().set_minimized(false);
    // A Wayland window appears with its first frame; shown from a callback, with
    // nothing else changed, nothing asks for one.
    ui.window().request_redraw();
}

/// Closing the window parks the app in the tray, as the Windows original does;
/// with no tray to find it in again, it quits.
pub fn on_close(tray: &TrayPresence) -> CloseRequestResponse {
    if !tray.shown() {
        let _ = slint::quit_event_loop();
    }
    CloseRequestResponse::HideWindow
}

struct Instance {
    window: slint::Weak<AppWindow>,
}

#[zbus::interface(name = "io.github.ykk.gglinux.App")]
impl Instance {
    /// Another launch: show the window, unless that one was a `--hidden` one too.
    fn activate(&self, hidden: bool) {
        eprintln!("gglinux: another launch (hidden: {hidden})");
        if !hidden {
            if let Err(e) = self.window.upgrade_in_event_loop(|ui| show_main(&ui)) {
                eprintln!("gglinux: asked to show the window, but {e}");
            }
        }
    }
}

pub enum Claim {
    /// This copy is the one; keep the connection, the name goes with it.
    First(zbus::Connection),
    /// Another copy runs and was told; this one should exit.
    Handed,
    /// No session bus, or a copy holding the name that doesn't answer: run
    /// anyway, without the guard.
    Unguarded(String),
}

/// Take the app's name on the session bus, or hand over to whoever has it.
pub async fn claim(window: slint::Weak<AppWindow>, hidden: bool) -> Claim {
    let name = bus_name();
    match tokio::time::timeout(BUS_TIMEOUT, take_name(&name, window)).await {
        Ok(Ok(Some(conn))) => Claim::First(conn),
        Ok(Ok(None)) => match tokio::time::timeout(BUS_TIMEOUT, hand_over(&name, hidden)).await {
            Ok(Ok(())) => Claim::Handed,
            Ok(Err(e)) => Claim::Unguarded(format!(
                "another gglinux holds {name} but didn't answer: {e}"
            )),
            Err(_) => Claim::Unguarded(format!("another gglinux holds {name} but didn't answer")),
        },
        Ok(Err(e)) => Claim::Unguarded(e.to_string()),
        Err(_) => Claim::Unguarded("the session bus didn't answer".into()),
    }
}

/// The name, or `None` if another copy has it. It's asked for with neither side
/// allowing a takeover (zbus's builder would let a second copy replace the
/// first) and without queueing.
async fn take_name(
    name: &str,
    window: slint::Weak<AppWindow>,
) -> zbus::Result<Option<zbus::Connection>> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    let conn = zbus::connection::Builder::session()?
        .serve_at(OBJECT, Instance { window })?
        .build()
        .await?;
    match conn
        .request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Ok(Some(conn)),
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

async fn hand_over(name: &str, hidden: bool) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(name), OBJECT, Some(INTERFACE), "Activate", &(hidden,))
        .await?;
    Ok(())
}

/// Whether the tray shows the icon now, as ksni hears from the
/// StatusNotifierWatcher; each change numbered, so a wait for the tray can tell
/// whether it came back meanwhile.
#[derive(Clone, Default)]
pub struct TrayPresence(Arc<Mutex<Presence>>);

#[derive(Default)]
struct Presence {
    shown: bool,
    changes: u64,
}

impl TrayPresence {
    pub fn shown(&self) -> bool {
        self.lock().shown
    }

    /// Record a change; its number.
    fn set(&self, shown: bool) -> u64 {
        let mut p = self.lock();
        p.shown = shown;
        p.changes += 1;
        p.changes
    }

    /// No tray since change `n`.
    fn gone_since(&self, n: u64) -> bool {
        let p = self.lock();
        !p.shown && p.changes == n
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Presence> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The tray icon: click it for the window; its menu shows the window or quits.
pub struct Tray {
    window: slint::Weak<AppWindow>,
    icon: Vec<ksni::Icon>,
    presence: TrayPresence,
}

impl Tray {
    /// The tray isn't there: unless it comes back within [`TRAY_GRACE`], open the
    /// window, so a hidden app isn't lost.
    fn missing(&self) {
        let n = self.presence.set(false);
        let presence = self.presence.clone();
        let window = self.window.clone();
        crate::client::runtime().spawn(async move {
            tokio::time::sleep(TRAY_GRACE).await;
            if presence.gone_since(n) {
                let _ = window.upgrade_in_event_loop(|ui| {
                    if !ui.window().is_visible() {
                        eprintln!("gglinux: no tray to wait in; opening the window");
                        show_main(&ui);
                    }
                });
            }
        });
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "gglinux".into()
    }
    fn title(&self) -> String {
        "gglinux".into()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "gglinux".into(),
            ..Default::default()
        }
    }
    fn icon_name(&self) -> String {
        // From the icon theme when installed; the pixmap below otherwise.
        "gglinux".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.window.upgrade_in_event_loop(|ui| show_main(&ui));
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Show gglinux".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.window.upgrade_in_event_loop(|ui| show_main(&ui));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                // The daemon keeps running: it's a service of its own.
                activate: Box::new(|_| {
                    let _ = slint::quit_event_loop();
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
    fn watcher_online(&self) {
        eprintln!("gglinux: back in the tray");
        self.presence.set(true);
    }
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        eprintln!("gglinux: no tray ({reason:?})");
        self.missing();
        // Keep the icon ready for when a tray (re)appears.
        true
    }
}

/// Put the icon in the tray, or wait for one to put it in. `Err`: no tray can
/// ever show it (no session bus), so the window has nowhere to hide.
pub async fn start_tray(
    window: slint::Weak<AppWindow>,
    presence: TrayPresence,
) -> Result<ksni::Handle<Tray>, String> {
    // Shown, unless the watcher turns out missing while it's registered.
    presence.set(true);
    let tray = Tray {
        window,
        icon: tray_icon(),
        presence: presence.clone(),
    };
    let spawned = tokio::time::timeout(BUS_TIMEOUT, tray.assume_sni_available(true).spawn()).await;
    let error = match spawned {
        Ok(Ok(handle)) => return Ok(handle),
        Ok(Err(e)) => e.to_string(),
        Err(_) => "the session bus didn't answer".into(),
    };
    presence.set(false);
    Err(error)
}

/// The tray's picture: ARGB32 in network byte order, as StatusNotifierItem wants.
pub fn tray_icon() -> Vec<ksni::Icon> {
    let Ok(img) = image::load_from_memory(include_bytes!("../../../data/icons/32x32.png")) else {
        return Vec::new();
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let data = rgba
        .pixels()
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect();
    vec![ksni::Icon {
        width: w as i32,
        height: h as i32,
        data,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_debug_build_does_not_hand_over_to_the_installed_copy() {
        // `cargo test --release` (the PKGBUILD's check()) builds the release name.
        let want = if cfg!(debug_assertions) {
            format!("{APP_ID}.dev")
        } else {
            APP_ID.to_string()
        };
        assert_eq!(bus_name(), want);
    }

    #[test]
    fn hidden_is_only_the_flag() {
        let args = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert!(hidden(&args("/usr/bin/gglinux --hidden")));
        assert!(!hidden(&args("/usr/bin/gglinux")));
        assert!(!hidden(&args("/opt/--hidden/gglinux")));
    }

    #[test]
    fn only_a_hidden_start_with_a_tray_stays_hidden() {
        assert!(!opens_at_start(true, true));
        assert!(opens_at_start(true, false));
        assert!(opens_at_start(false, true));
        assert!(opens_at_start(false, false));
    }

    #[test]
    fn a_tray_back_in_time_calls_off_the_wait() {
        let p = TrayPresence::default();
        assert!(!p.shown());
        p.set(true);
        let n = p.set(false);
        assert!(p.gone_since(n));
        p.set(true);
        assert!(p.shown() && !p.gone_since(n));
        // Gone again: the earlier wait is stale, the new one counts.
        let m = p.set(false);
        assert!(!p.gone_since(n) && p.gone_since(m));
    }

    #[test]
    fn the_tray_icon_is_argb() {
        let icons = tray_icon();
        assert_eq!(icons.len(), 1);
        let i = &icons[0];
        assert_eq!(i.data.len(), (i.width * i.height * 4) as usize);
    }

    /// Only where there's no session bus to reach (the PC's test runs point
    /// XDG_RUNTIME_DIR at a folder of their own and unset the bus): elsewhere
    /// this would take the debug name on the desktop's bus.
    fn no_session_bus() -> bool {
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) else {
            return false; // zbus would try /run/user/<uid>/bus
        };
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none()
            && !std::path::Path::new(&dir).join("bus").exists()
    }

    #[test]
    fn without_a_session_bus_the_app_runs_unguarded_with_no_tray() {
        if !no_session_bus() {
            return;
        }
        let rt = crate::client::runtime();
        let started = std::time::Instant::now();
        assert!(matches!(
            rt.block_on(claim(slint::Weak::default(), true)),
            Claim::Unguarded(_)
        ));
        let presence = TrayPresence::default();
        assert!(rt
            .block_on(start_tray(slint::Weak::default(), presence.clone()))
            .is_err());
        // So closing the window quits, and a hidden start opens it.
        assert!(!presence.shown());
        assert!(opens_at_start(true, false));
        assert!(started.elapsed() < BUS_TIMEOUT);
    }
}
