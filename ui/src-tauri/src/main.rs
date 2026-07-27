//! gglinux desktop UI.
//!
//! The webview cannot open a unix socket, so this process owns the connection to
//! `gglinuxd` and acts as a relay: every NDJSON line the daemon emits is forwarded
//! to the frontend as a `daemon://message` event, and the `daemon_send` command
//! pushes request frames back down. The daemon is the source of truth; this side
//! keeps no device state of its own and reconnects on its own if the daemon
//! restarts.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

fn main() {
    let bridge = Arc::new(Bridge::default());

    tauri::Builder::default()
        .manage(Arc::clone(&bridge))
        .invoke_handler(tauri::generate_handler![daemon_status, daemon_send])
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
