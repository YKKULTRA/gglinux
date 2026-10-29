// SPDX-License-Identifier: GPL-3.0-or-later
//! `gglinux`: the desktop app. `gglinux --hidden` starts it in the tray.

use gglinux::shell::{self, Claim};
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    let args: Vec<String> = std::env::args().collect();
    let hidden = shell::hidden(&args);
    let ui = gglinux::AppWindow::new()?;
    // The Wayland app id names the window after the desktop entry, for its icon
    // and for the portals. After `new` (which sets the platform up), before the
    // window is made at `show`.
    slint::set_xdg_app_id(shell::APP_ID)?;
    let rt = gglinux::client::runtime();

    // One copy at a time. Settled first, so a second launch only hands over: it
    // never connects to the daemon or puts up an icon.
    let _bus = match rt.block_on(shell::claim(ui.as_weak(), hidden)) {
        Claim::First(conn) => Some(conn),
        Claim::Handed => {
            eprintln!("gglinux: already running; asked it to show its window");
            return Ok(());
        }
        Claim::Unguarded(why) => {
            eprintln!("gglinux: running without the one-copy guard: {why}");
            None
        }
    };

    let presence = shell::TrayPresence::default();
    // Kept to the end: the icon goes with it.
    let tray = match rt.block_on(shell::start_tray(ui.as_weak(), presence.clone())) {
        Ok(handle) => Some(handle),
        Err(e) => {
            eprintln!("gglinux: no tray icon ({e}); closing the window quits");
            None
        }
    };

    let _client = gglinux::start(&ui);
    ui.window()
        .on_close_requested(move || shell::on_close(&presence));
    if shell::opens_at_start(hidden, tray.is_some()) {
        ui.show()?;
    }
    // Returns on the tray's Quit, or when the window closes with no tray.
    slint::run_event_loop_until_quit()
}
