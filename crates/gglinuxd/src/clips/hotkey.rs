// SPDX-License-Identifier: GPL-3.0-or-later
//! The save-clip hotkey, through the GlobalShortcuts portal: the daemon never
//! sees the keyboard (reactive lighting's security choice), so the desktop does.
//! KDE asks to confirm Alt+S; Hyprland binds portal shortcuts only from its own
//! config, so there the user adds a `global` bind for [`GLOBAL_NAME`].

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::Internal;

const ID: &str = "save-clip";
/// The app id the portal shows and remembers the binding under. The portal
/// accepts it only if a desktop entry of that name is installed.
const APP_ID: &str = "io.github.ykk.gglinux";
const DESKTOP_ENTRY: &str = "io.github.ykk.gglinux.desktop";
/// The shortcut's full name, as Hyprland's `global` dispatcher takes it.
const GLOBAL_NAME: &str = "io.github.ykk.gglinux:save-clip";

pub async fn run(internal: mpsc::UnboundedSender<Internal>) {
    if let Err(e) = bind(&internal).await {
        warn!(error = %e, "no clip hotkey");
        let _ = internal.send(Internal::Hotkey(Err(e)));
    }
}

async fn bind(internal: &mpsc::UnboundedSender<Internal>) -> Result<(), String> {
    // A host (non-Flatpak) app names itself to the portal. Portals since 1.20
    // refuse a shortcut session without that name; older ones lack the
    // registry and bind anyway.
    let registered = match ashpd::AppID::try_from(APP_ID) {
        Ok(id) => match ashpd::register_host_app(id).await {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "the desktop portal did not take gglinux's app id");
                false
            }
        },
        Err(_) => false,
    };
    let fail = |e: ashpd::Error| failure(&e.to_string(), registered);
    let portal = GlobalShortcuts::new().await.map_err(fail)?;
    let session = OpenSession(Some(
        portal
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(fail)?,
    ));
    let shortcut = NewShortcut::new(ID, "Save the last clip").preferred_trigger("ALT+S");
    let request = portal
        .bind_shortcuts(
            session.get(),
            &[shortcut],
            None,
            BindShortcutsOptions::default(),
        )
        .await
        .map_err(fail)?;
    let bound = request.response().map_err(fail)?;
    let trigger = outcome(
        bound
            .shortcuts()
            .iter()
            .find(|s| s.id() == ID)
            .map(|s| s.trigger_description()),
        &desktop(),
    );
    match &trigger {
        Ok(trigger) => info!(%trigger, "clip hotkey bound"),
        Err(e) => warn!(error = %e, "no clip hotkey"),
    }
    let _ = internal.send(Internal::Hotkey(trigger));
    let mut activations = portal.receive_activated().await.map_err(fail)?;
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

/// What the portal bound, for the UI: the key, or why there's none. KDE binds
/// the shortcut with no key when Alt+S is taken, or when the owner clears it in
/// its dialog; Hyprland always does, and takes the key from its config. The
/// session stays open either way, so a key set later still saves.
fn outcome(trigger: Option<&str>, desktop: &str) -> Result<String, String> {
    match trigger {
        Some("") => Err(no_key(desktop)),
        trigger => Ok(trigger.unwrap_or("bound").to_string()),
    }
}

/// Where to give the shortcut a key, on this desktop.
fn no_key(desktop: &str) -> String {
    let desktop = desktop.to_ascii_lowercase();
    if desktop.contains("hyprland") {
        format!(
            "Hyprland takes the key from its config: add hl.bind(\"ALT + S\", hl.dsp.global(\"{GLOBAL_NAME}\")) \
             to hyprland.lua, or bind = ALT, S, global, {GLOBAL_NAME} to hyprland.conf"
        )
    } else if desktop.contains("kde") {
        "KDE gave it no key; set one in System Settings → Shortcuts".into()
    } else {
        format!("the desktop gave it no key; bind {GLOBAL_NAME} in its shortcut settings")
    }
}

/// The desktop the daemon runs under: `XDG_CURRENT_DESKTOP`, which the user
/// manager gets from the session, or Hyprland's own variable if that's missing.
fn desktop() -> String {
    match std::env::var("XDG_CURRENT_DESKTOP") {
        Ok(d) if !d.is_empty() => d,
        _ if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() => "Hyprland".into(),
        _ => String::new(),
    }
}

/// A failure to bind, with the likely cause when the portal didn't take the app id.
fn failure(error: &str, registered: bool) -> String {
    if registered {
        error.to_owned()
    } else {
        format!(
            "{error} (the desktop portal knows gglinux by its app-menu entry, {DESKTOP_ENTRY}: \
             reinstall gglinux if it's missing)"
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_bound_with_no_key_is_no_hotkey() {
        assert_eq!(outcome(Some("Alt+S"), "KDE"), Ok("Alt+S".into()));
        assert!(
            outcome(Some(""), "KDE").is_err_and(|e| e.contains("System Settings")),
            "KDE asked already; nothing to wait for"
        );
    }

    #[test]
    fn on_hyprland_the_message_is_the_bind_line_for_its_config() {
        let e = outcome(Some(""), "Hyprland").unwrap_err();
        assert!(
            e.contains(r#"hl.dsp.global("io.github.ykk.gglinux:save-clip")"#),
            "{e}"
        );
        assert!(
            e.contains("bind = ALT, S, global, io.github.ykk.gglinux:save-clip"),
            "{e}"
        );
        let other = outcome(Some(""), "sway").unwrap_err();
        assert!(other.contains("io.github.ykk.gglinux:save-clip"), "{other}");
    }

    #[test]
    fn a_failure_without_the_app_id_points_at_the_desktop_entry() {
        assert_eq!(failure("boom", true), "boom");
        assert!(failure("An app id is required", false).contains("io.github.ykk.gglinux.desktop"));
    }
}
