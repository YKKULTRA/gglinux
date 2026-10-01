//! gglinux desktop UI.
//!
//! The webview cannot open a unix socket, so this process owns the connection to
//! `gglinuxd` and acts as a relay: every NDJSON line the daemon emits is forwarded
//! to the frontend as a `daemon://message` event, and the `daemon_send` command
//! pushes request frames back down. The daemon is the source of truth; this side
//! keeps no device state of its own and reconnects on its own if the daemon
//! restarts.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Only its tests use it until the Tauri commands for it are wired up.
#[allow(dead_code)]
mod system;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::Mutex;

/// One daemon message, forwarded verbatim.
const EV_MESSAGE: &str = "daemon://message";
/// `{ "connected": bool }`, emitted only on transitions.
const EV_STATUS: &str = "daemon://status";
/// How long to wait before retrying a dropped or refused connection.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Holds the write half of the daemon socket on behalf of the webview.
#[derive(Default)]
struct Bridge {
    writer: Mutex<Option<OwnedWriteHalf>>,
    connected: AtomicBool,
}

#[tauri::command]
fn daemon_status(bridge: State<'_, Arc<Bridge>>) -> bool {
    bridge.connected.load(Ordering::Relaxed)
}

#[tauri::command]
async fn daemon_send(
    bridge: State<'_, Arc<Bridge>>,
    frame: serde_json::Value,
) -> Result<(), String> {
    let mut guard = bridge.writer.lock().await;
    let writer = guard
        .as_mut()
        .ok_or_else(|| "gglinuxd is not connected".to_string())?;

    let mut line = serde_json::to_vec(&frame).map_err(|e| e.to_string())?;
    line.push(b'\n');
    writer.write_all(&line).await.map_err(|e| e.to_string())
}

/// Open a folder in the file manager; only existing directories, given as
/// absolute paths so `xdg-open` can't mistake one for an option.
#[tauri::command]
fn open_folder(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.is_absolute() || !p.is_dir() {
        return Err(format!("{path} is not a folder"));
    }
    let mut child = std::process::Command::new("xdg-open")
        .arg(p)
        .spawn()
        .map_err(|e| e.to_string())?;
    // Reap it when it exits, so each click doesn't leave a zombie behind.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Connect, pump lines to the frontend, and retry forever.
async fn bridge_loop(app: AppHandle, bridge: Arc<Bridge>) {
    let path = gg_ipc::socket_path();
    loop {
        if let Ok(stream) = UnixStream::connect(&path).await {
            let (reader, writer) = stream.into_split();
            *bridge.writer.lock().await = Some(writer);
            set_connected(&app, &bridge, true);

            let mut lines = BufReader::new(reader).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => match serde_json::from_str::<serde_json::Value>(&line) {
                        Ok(value) => {
                            let _ = app.emit(EV_MESSAGE, value);
                        }
                        Err(e) => eprintln!("gglinuxd sent an unparsable line: {e}"),
                    },
                    Ok(None) => break, // daemon closed the socket
                    Err(e) => {
                        eprintln!("gglinuxd read error: {e}");
                        break;
                    }
                }
            }

            *bridge.writer.lock().await = None;
            set_connected(&app, &bridge, false);
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Emit a status event only when the flag actually flips, so the UI doesn't churn.
fn set_connected(app: &AppHandle, bridge: &Bridge, connected: bool) {
    if bridge.connected.swap(connected, Ordering::Relaxed) != connected {
        let _ = app.emit(EV_STATUS, serde_json::json!({ "connected": connected }));
    }
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show gglinux", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    TrayIconBuilder::with_id("gglinux")
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("bundled window icon"),
        )
        .tooltip("gglinux")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Work around webkit2gtk's DMABUF renderer on the NVIDIA proprietary driver.
///
/// WebKit cannot allocate its GBM buffers there. Under a native Wayland session
/// that kills the process outright — `Gdk-Message: Error 71 (Protocol error)` —
/// and under XWayland it survives but logs `Failed to create GBM buffer` and
/// renders nothing. Disabling the DMABUF renderer costs some compositing
/// performance and is the workaround upstream recommends.
///
/// Scoped as narrowly as the fix allows: only when that driver is loaded, and
/// never over a value the user set themselves.
fn disable_dmabuf_renderer_on_nvidia() {
    const VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    if std::env::var_os(VAR).is_some() {
        return;
    }
    // nouveau is unaffected, so probe for the proprietary module specifically.
    if std::path::Path::new("/sys/module/nvidia").exists() {
        std::env::set_var(VAR, "1");
    }
}

fn main() {
    // Before the builder: WebKit reads this when it initialises, and setting an
    // env var is only sound while the process is still single-threaded.
    disable_dmabuf_renderer_on_nvidia();

    let bridge = Arc::new(Bridge::default());

    tauri::Builder::default()
        .manage(Arc::clone(&bridge))
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            daemon_send,
            open_folder
        ])
        .setup(move |app| {
            build_tray(app.handle())?;
            tauri::async_runtime::spawn(bridge_loop(app.handle().clone(), bridge));
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window parks gglinux in the tray rather than quitting,
            // matching what the Windows original does.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start the gglinux UI");
}

#[cfg(test)]
mod tests {
    use super::open_folder;

    #[test]
    fn open_folder_refuses_anything_but_a_folder() {
        // Refusals only: a folder that passed would open the file manager.
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        for path in ["", "/no/such/folder", file] {
            assert!(open_folder(path.into()).is_err(), "{path:?} was accepted");
        }
    }
}
