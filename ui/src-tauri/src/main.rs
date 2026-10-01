//! gglinux desktop UI.
//!
//! The webview cannot open a unix socket, so this process owns the connection to
//! `gglinuxd` and acts as a relay: every NDJSON line the daemon emits is forwarded
//! to the frontend as a `daemon://message` event, and the `daemon_send` command
//! pushes request frames back down. The daemon is the source of truth; this side
//! keeps no device state of its own and reconnects on its own if the daemon
//! restarts.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod system;

use std::path::PathBuf;
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

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `$XDG_CONFIG_HOME/autostart` (so usually `~/.config/autostart`): the folder
/// beside gglinux's own config folder.
fn autostart_dir() -> PathBuf {
    gg_ipc::config_dir().with_file_name("autostart")
}

/// Run a program to the end; when it fails, its stderr is the error.
fn run_quietly(argv: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!("{} {}", argv[0], out.status)
    } else {
        err
    })
}

#[tauri::command]
fn get_autostart() -> bool {
    system::autostart_on(&autostart_dir())
}

/// Off the main thread, like `import_settings`: `systemctl` can take a moment.
#[tauri::command(async)]
fn set_autostart(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = system::installed_exe(&exe);
    system::set_autostart(&autostart_dir(), &exe, enabled, &run_quietly)
}

/// Write a backup of every setting; returns its path.
#[tauri::command]
fn export_settings() -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let bundle = system::export(&gg_ipc::config_dir(), now);
    let path =
        system::export_dir(&home()).join(format!("gglinux-backup-{}.json", system::stamp(now)));
    let text = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Restore a backup and restart the daemon so it loads it; returns what
/// happened, for the UI. Off the main thread, since the restart waits for the
/// daemon to stop and start again.
#[tauri::command(async)]
fn import_settings(text: String) -> Result<String, String> {
    let wrote = system::import(&gg_ipc::config_dir(), &text)?;
    if wrote.is_empty() {
        return Ok("The backup holds no settings, so nothing changed.".into());
    }
    let restarted =
        run_quietly(&["systemctl", "--user", "try-restart", "gglinuxd.service"]).is_ok();
    Ok(format!(
        "Restored {}. {}",
        wrote.join(", "),
        if restarted {
            "gglinuxd restarted to load them."
        } else {
            "Restart gglinuxd to load them."
        }
    ))
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

/// Autostart launches gglinux with `--hidden`: stay in the tray until asked.
fn hidden(args: &[String]) -> bool {
    args.iter().any(|a| a == "--hidden")
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
        // Registered first, as the plugin asks. A second launch (the app menu
        // while the autostarted copy sits in the tray) hands its arguments to
        // the running copy over D-Bus and exits, so there's only ever one tray
        // icon and one daemon connection. The running copy shows its window,
        // unless that launch was a --hidden one too.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !hidden(&args) {
                show_main(app);
            }
        }))
        .manage(Arc::clone(&bridge))
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            daemon_send,
            open_folder,
            get_autostart,
            set_autostart,
            export_settings,
            import_settings
        ])
        .setup(move |app| {
            build_tray(app.handle())?;
            // The window starts invisible.
            if !hidden(&std::env::args().collect::<Vec<_>>()) {
                show_main(app.handle());
            }
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
    use super::{hidden, open_folder};

    #[test]
    fn only_hidden_launches_stay_in_the_tray() {
        // Arguments as both launches see them: the program path first.
        let args = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert!(hidden(&args("/usr/bin/gglinux --hidden")));
        assert!(!hidden(&args("/usr/bin/gglinux")));
        assert!(!hidden(&args("/opt/--hidden/gglinux")));
    }

    #[test]
    fn open_folder_refuses_anything_but_a_folder() {
        // Refusals only: a folder that passed would open the file manager.
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        for path in ["", "/no/such/folder", file] {
            assert!(open_folder(path.into()).is_err(), "{path:?} was accepted");
        }
    }
}
