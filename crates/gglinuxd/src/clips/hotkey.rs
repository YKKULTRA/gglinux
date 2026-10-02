//! The save-clip hotkey, through the GlobalShortcuts portal: the daemon never
//! sees the keyboard (reactive lighting's security choice), so KDE does.

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::Internal;

const ID: &str = "save-clip";
/// The app id the portal shows and remembers the binding under.
const APP_ID: &str = "io.github.ykk.gglinux";

pub async fn run(internal: mpsc::UnboundedSender<Internal>) {
    if let Err(e) = bind(&internal).await {
        warn!(error = %e, "no clip hotkey");
        let _ = internal.send(Internal::Hotkey(Err(e.to_string())));
    }
}

async fn bind(internal: &mpsc::UnboundedSender<Internal>) -> ashpd::Result<()> {
    // A host (non-Flatpak) app names itself to the portal; older portals lack
    // the registry, and binding still works there, so a failure is ignored.
    if let Ok(id) = ashpd::AppID::try_from(APP_ID) {
        let _ = ashpd::register_host_app(id).await;
    }
    let portal = GlobalShortcuts::new().await?;
    let session = OpenSession(Some(portal.create_session(CreateSessionOptions::default()).await?));
    let shortcut = NewShortcut::new(ID, "Save the last clip").preferred_trigger("ALT+S");
    let request = portal.bind_shortcuts(session.get(), &[shortcut], None, BindShortcutsOptions::default()).await?;
    let bound = request.response()?;
    let trigger = bound
        .shortcuts()
        .iter()
        .find(|s| s.id() == ID)
        .map(|s| s.trigger_description().to_string())
        .unwrap_or_else(|| "bound".into());
    info!(%trigger, "clip hotkey bound");
    let _ = internal.send(Internal::Hotkey(Ok(trigger)));
    let mut activations = portal.receive_activated().await?;
    while let Some(a) = activations.next().await {
        if a.shortcut_id() == ID {
            let _ = internal.send(Internal::SaveFromHotkey);
        }
    }
    // Nothing listens for the key any more: say so, rather than show it as bound.
    warn!("the clip hotkey's portal connection closed");
    let _ = internal.send(Internal::Hotkey(Err("the portal connection closed".into())));
    Ok(())
}

/// Closes the portal session when this task ends, aborted or not. ashpd
/// doesn't, and its D-Bus connection lives as long as the daemon, so an open
/// session would keep the key grabbed after clips are turned off, and turning
/// them back on would bind it twice.
struct OpenSession(Option<Session<GlobalShortcuts>>);

impl OpenSession {
    fn get(&self) -> &Session<GlobalShortcuts> {
        self.0.as_ref().expect("open until dropped")
    }
}

impl Drop for OpenSession {
    fn drop(&mut self) {
        if let (Some(session), Ok(rt)) = (self.0.take(), tokio::runtime::Handle::try_current()) {
            rt.spawn(async move {
                let _ = session.close().await;
            });
        }
    }
}
