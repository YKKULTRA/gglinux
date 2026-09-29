//! The save-clip hotkey, through the GlobalShortcuts portal.

use tokio::sync::mpsc;

use super::Internal;

pub async fn run(internal: mpsc::UnboundedSender<Internal>) {
    let _ = internal.send(Internal::Hotkey(Err("not wired yet".into())));
}
