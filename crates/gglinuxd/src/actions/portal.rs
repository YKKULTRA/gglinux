// SPDX-License-Identifier: GPL-3.0-or-later
//! The spare keys through the desktop's GlobalShortcuts portal, in a session of the
//! actions' own (the clip hotkey has its own, in `clips/hotkey.rs`). Each bound key is
//! a shortcut `action-f13` … `action-f24` of the app `io.github.ykk.gglinux`.
//!
//! A portal session binds its shortcuts once, so a new set of keys is bound in a new
//! session, and the old one is closed only after the new one is bound: if the desktop
//! refuses, the old keys keep working. KDE asks the owner to confirm keys it hasn't
//! bound for gglinux before; Hyprland binds portal shortcuts only from its config.

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use gg_ipc::ActionBinding;
use tokio::sync::{mpsc, Mutex, OnceCell};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// The app id the portal shows and remembers the keys under; it needs the desktop
/// entry of that name.
const APP_ID: &str = "io.github.ykk.gglinux";

/// The shortcut id of a spare key: `action-f13` for F13.
pub fn id(key: u8) -> Option<String> {
    ActionBinding::key_name(key).map(|name| format!("action-{}", name.to_ascii_lowercase()))
}

/// The spare key of a shortcut id.
pub fn key(id: &str) -> Option<u8> {
    (0x68..=0x73).find(|&k| self::id(k).as_deref() == Some(id))
}

/// One key to bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub id: String,
    /// What the desktop lists it as: the action's name.
    pub description: String,
    /// The key it should have: the spare key itself.
    pub trigger: String,
}

impl Shortcut {
    pub fn of(b: &ActionBinding) -> Option<Shortcut> {
        Some(Shortcut {
            id: id(b.key)?,
            description: b.label.clone(),
            trigger: ActionBinding::key_name(b.key)?,
        })
    }
}

/// What the desktop bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    /// The ids it took (KDE's dialog can leave some out).
    pub ids: Vec<String>,
    /// What it said, for Settings.
    pub status: String,
}

/// The desktop's shortcuts, as the actions use them. Tests use a fake, so they never
/// touch D-Bus.
pub trait Portal: Send + Sync {
    /// Bind `shortcuts` in a new session, which replaces the one bound before once
    /// it's bound; none closes the session. It may show the desktop's dialog. Keys
    /// pressed from then on come in through the channel the portal was made with.
    fn bind(&self, shortcuts: Vec<Shortcut>) -> BoxFuture<'_, Result<Bound, String>>;
}

/// What Settings shows for what the desktop bound: `(id, key)` pairs, the key empty
/// when it gave none.
pub fn status(bound: &[(String, String)], desktop: &str) -> String {
    let name = |id: &String| {
        key(id)
            .and_then(ActionBinding::key_name)
            .unwrap_or_else(|| id.clone())
    };
    let names: Vec<String> = bound.iter().map(|(id, _)| name(id)).collect();
    if bound.is_empty() {
        return "no keys bound".into();
    }
    if bound.iter().all(|(_, trigger)| trigger.is_empty()) {
        return if desktop.to_ascii_lowercase().contains("hyprland") {
            format!("bound: Hyprland takes {} from its config", names.join(", "))
        } else {
            format!(
                "bound with no keys yet: give {} their keys in the desktop's shortcut settings",
                names.join(", ")
            )
        };
    }
    let each: Vec<String> = bound
        .iter()
        .zip(&names)
        .map(|((_, trigger), name)| match trigger.as_str() {
            "" => format!("{name} → no key yet"),
            t => format!("{name} → {t}"),
        })
        .collect();
    format!("bound: {}", each.join(", "))
}

/// The desktop the daemon runs under, as in `clips/hotkey.rs`.
pub fn desktop() -> String {
    match std::env::var("XDG_CURRENT_DESKTOP") {
        Ok(d) if !d.is_empty() => d,
        _ if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() => "Hyprland".into(),
        _ => String::new(),
    }
}

/// The real [`Portal`], over `xdg-desktop-portal`.
pub struct DesktopPortal {
    activated: mpsc::UnboundedSender<String>,
    live: Mutex<Option<Live>>,
    registered: OnceCell<bool>,
}

impl DesktopPortal {
    /// Keys pressed go to `activated`, as their shortcut ids.
    pub fn new(activated: mpsc::UnboundedSender<String>) -> DesktopPortal {
        DesktopPortal {
            activated,
            live: Mutex::new(None),
            registered: OnceCell::new(),
        }
    }

    async fn bind_now(&self, shortcuts: Vec<Shortcut>) -> Result<Bound, String> {
        let mut live = self.live.lock().await;
        if shortcuts.is_empty() {
            *live = None;
            return Ok(Bound {
                ids: Vec::new(),
                status: status(&[], ""),
            });
        }
        let registered = *self.registered.get_or_init(register).await;
        let fail = |e: ashpd::Error| failure(&e.to_string(), registered);
        let portal = GlobalShortcuts::new().await.map_err(fail)?;
        let session = portal
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(fail)?;
        let new: Vec<NewShortcut> = shortcuts
            .iter()
            .map(|s| NewShortcut::new(&s.id, &s.description).preferred_trigger(s.trigger.as_str()))
            .collect();
        let answer = match portal
            .bind_shortcuts(&session, &new, None, BindShortcutsOptions::default())
            .await
        {
            Ok(request) => request.response(),
            Err(e) => Err(e),
        };
        let answer = match answer {
            Ok(answer) => answer,
            Err(e) => {
                // The old session, if any, stays as it was.
                let _ = session.close().await;
                return Err(fail(e));
            }
        };
        let wanted: Vec<&str> = shortcuts.iter().map(|s| s.id.as_str()).collect();
        let pairs: Vec<(String, String)> = answer
            .shortcuts()
            .iter()
            .filter(|s| wanted.contains(&s.id()))
            .map(|s| (s.id().to_owned(), s.trigger_description().to_owned()))
            .collect();
        let mut activations = portal.receive_activated().await.map_err(fail)?;
        // Another session of this connection (the clip hotkey's, or the one being
        // replaced) hears the same signals: only this one's count.
        let path = serde_json::to_value(&session)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned));
        let tx = self.activated.clone();
        let listener = tokio::spawn(async move {
            while let Some(a) = activations.next().await {
                let ours = path
                    .as_deref()
                    .is_none_or(|p| a.session_handle().as_str() == p);
                if ours {
                    let _ = tx.send(a.shortcut_id().to_owned());
                }
            }
            warn!("the actions' portal connection closed");
        });
        let bound = Bound {
            ids: pairs.iter().map(|(id, _)| id.clone()).collect(),
            status: status(&pairs, &desktop()),
        };
        info!(status = %bound.status, "action keys bound");
        // Dropping the old one closes its session.
        *live = Some(Live {
            session: Some(session),
            listener,
        });
        Ok(bound)
    }
}

impl Portal for DesktopPortal {
    fn bind(&self, shortcuts: Vec<Shortcut>) -> BoxFuture<'_, Result<Bound, String>> {
        Box::pin(self.bind_now(shortcuts))
    }
}

/// A host (non-Flatpak) app names itself to the portal; portals since 1.20 refuse a
/// shortcut session without that name. The clip hotkey may have done so already on
/// the same connection, which the portal refuses as a second time: that's fine too.
async fn register() -> bool {
    let Ok(id) = ashpd::AppID::try_from(APP_ID) else {
        return false;
    };
    match ashpd::register_host_app(id).await {
        Ok(()) => true,
        Err(e) => {
            warn!(error = %e, "the desktop portal did not take gglinux's app id for the actions");
            false
        }
    }
}

/// A failure to bind, with the likely cause when the portal didn't take the app id.
fn failure(error: &str, registered: bool) -> String {
    if registered {
        format!("the desktop didn't bind the keys: {error}")
    } else {
        format!(
            "the desktop didn't bind the keys: {error} (the desktop portal knows gglinux by its \
             app-menu entry, {APP_ID}.desktop: reinstall gglinux if it's missing)"
        )
    }
}

/// A bound session and the task passing its keys on. Dropped, it closes the session:
/// ashpd doesn't, and its D-Bus connection lives as long as the daemon.
struct Live {
    session: Option<Session<GlobalShortcuts>>,
    listener: JoinHandle<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.listener.abort();
        if let (Some(session), Ok(rt)) =
            (self.session.take(), tokio::runtime::Handle::try_current())
        {
            rt.spawn(async move {
                let _ = session.close().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_spare_key_has_its_shortcut_id_and_back() {
        assert_eq!(id(0x68).as_deref(), Some("action-f13"));
        assert_eq!(id(0x73).as_deref(), Some("action-f24"));
        assert_eq!(id(0x67), None);
        for k in 0x68..=0x73 {
            assert_eq!(key(&id(k).unwrap()), Some(k));
        }
        for bad in [
            "action-f12",
            "action-f25",
            "action-f013",
            "save-clip",
            "action-fx",
            "",
        ] {
            assert_eq!(key(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_status_says_which_keys_the_desktop_gave() {
        let pair = |id: &str, t: &str| (id.to_string(), t.to_string());
        assert_eq!(
            status(&[pair("action-f13", "F13"), pair("action-f14", "")], "KDE"),
            "bound: F13 → F13, F14 → no key yet"
        );
        assert_eq!(
            status(
                &[pair("action-f13", ""), pair("action-f15", "")],
                "Hyprland"
            ),
            "bound: Hyprland takes F13, F15 from its config"
        );
        assert!(status(&[pair("action-f13", "")], "sway").contains("shortcut settings"));
        assert_eq!(status(&[], "KDE"), "no keys bound");
    }

    #[test]
    fn a_failure_without_the_app_id_points_at_the_desktop_entry() {
        assert_eq!(
            failure("boom", true),
            "the desktop didn't bind the keys: boom"
        );
        assert!(failure("An app id is required", false).contains("io.github.ykk.gglinux.desktop"));
    }
}
