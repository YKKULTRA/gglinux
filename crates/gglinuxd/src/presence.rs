// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether anybody is at the computer, for screen care: the keyboard's screen rests
//! dark while nobody is.
//!
//! Nobody is there when no session of this user on a seat is both active and
//! unlocked (logind: locked, idle, switched away from, or nobody logged in, as at the
//! login screen when the daemon starts at boot), when the screensaver service says
//! the screen is locked (`org.freedesktop.ScreenSaver.GetActive`: Hyprland's hypridle
//! answers it but never tells logind; Plasma's locker does both), or while the
//! computer goes to sleep. Polled every [`POLL`], except sleep, which logind
//! announces: a delay lock holds the sleep back until the screen has gone dark.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use tracing::{debug, info, warn};
use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue};
use zbus::{Connection, MatchRule, MessageStream};

use crate::keyboard::{self, Command as KeyboardCommand};

const POLL: Duration = Duration::from_secs(2);
const LOGIN1: &str = "org.freedesktop.login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

/// Watch, and tell the keyboard worker whenever it changes.
pub fn spawn(keyboard: keyboard::Handle) {
    tokio::spawn(async move {
        let system = Connection::system()
            .await
            .map_err(|e| warn!(error = %e, "no system bus: the keyboard screen can't tell when nobody's there"))
            .ok();
        let session = Connection::session()
            .await
            .map_err(|e| debug!(error = %e, "no session bus: the screensaver can't be asked"))
            .ok();
        let mut sleeps = match &system {
            Some(c) => sleep_signals(c)
                .await
                .map_err(|e| debug!(error = %e, "can't hear logind's sleep announcements"))
                .ok(),
            None => None,
        };
        let mut lock = match &system {
            Some(c) => delay_sleep(c).await,
            None => None,
        };
        let mut sleeping = false;
        let mut told = None;
        let mut tick = tokio::time::interval(POLL);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                going = next_sleep(&mut sleeps) => match going {
                    Some(going) => sleeping = going,
                    None => sleeps = None, // the stream ended; polling still works
                },
            }
            let away = sleeping || !present(system.as_ref(), session.as_ref()).await;
            if told != Some(away) {
                if away {
                    info!("nobody at the computer: the keyboard screen rests");
                } else if told.is_some() {
                    info!("somebody's back at the computer");
                }
                // Answered once the screen shows it.
                if let Err(e) = keyboard
                    .send::<()>(|reply| KeyboardCommand::SetAway { away, reply })
                    .await
                {
                    debug!(error = %e, "the keyboard worker didn't take the presence");
                }
                told = Some(away);
            }
            if sleeping {
                lock = None; // dark now, so the computer may sleep
            } else if lock.is_none() {
                if let Some(c) = &system {
                    lock = delay_sleep(c).await;
                }
            }
        }
    });
}

async fn next_sleep(stream: &mut Option<MessageStream>) -> Option<bool> {
    match stream {
        Some(s) => loop {
            match s.next().await? {
                Ok(msg) => match msg.body().deserialize::<bool>() {
                    Ok(going) => return Some(going),
                    Err(e) => debug!(error = %e, "an unreadable PrepareForSleep"),
                },
                Err(e) => debug!(error = %e, "a bad message from logind"),
            }
        },
        None => std::future::pending().await,
    }
}

async fn sleep_signals(system: &Connection) -> zbus::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(LOGIN1)?
        .interface(MANAGER)?
        .member("PrepareForSleep")?
        .build();
    MessageStream::for_match_rule(rule, system, None).await
}

/// A logind delay lock on sleep: held, sleep waits (up to logind's limit, 5 s by
/// default) until it's let go.
async fn delay_sleep(system: &Connection) -> Option<OwnedFd> {
    let reply = system
        .call_method(
            Some(LOGIN1),
            "/org/freedesktop/login1",
            Some(MANAGER),
            "Inhibit",
            &(
                "sleep",
                "gglinux",
                "Darkening the keyboard screen first",
                "delay",
            ),
        )
        .await;
    match reply.and_then(|r| r.body().deserialize::<OwnedFd>()) {
        Ok(fd) => Some(fd),
        Err(e) => {
            debug!(error = %e, "no sleep delay lock");
            None
        }
    }
}

/// Somebody is there: a session of this user on a seat is active and unlocked, and the
/// screensaver isn't on. Without the system bus there's no telling, so yes.
async fn present(system: Option<&Connection>, session: Option<&Connection>) -> bool {
    let Some(system) = system else {
        return true;
    };
    let seated = match seat_sessions(system).await {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "couldn't ask logind for the sessions");
            return true;
        }
    };
    let mut any = false;
    for path in seated {
        match session_props(system, &path).await {
            Ok(mut props) => any |= in_use(&mut props),
            Err(e) => debug!(error = %e, "couldn't read a session"),
        }
    }
    any && !screensaver_on(session).await
}

/// This user's sessions on a seat (not SSH, not the user manager).
async fn seat_sessions(system: &Connection) -> zbus::Result<Vec<OwnedObjectPath>> {
    let reply = system
        .call_method(
            Some(LOGIN1),
            "/org/freedesktop/login1",
            Some(MANAGER),
            "ListSessions",
            &(),
        )
        .await?;
    let sessions: Vec<(String, u32, String, String, OwnedObjectPath)> =
        reply.body().deserialize()?;
    // SAFETY: getuid has no preconditions and can't fail.
    let me = unsafe { libc::getuid() };
    Ok(sessions
        .into_iter()
        .filter(|(_, uid, _, seat, _)| *uid == me && !seat.is_empty())
        .map(|(.., path)| path)
        .collect())
}

async fn session_props(
    system: &Connection,
    path: &OwnedObjectPath,
) -> zbus::Result<HashMap<String, OwnedValue>> {
    let reply = system
        .call_method(
            Some(LOGIN1),
            path.as_ref(),
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &("org.freedesktop.login1.Session",),
        )
        .await?;
    reply.body().deserialize()
}

/// A session somebody is using: active, not locked, not idle.
fn in_use(props: &mut HashMap<String, OwnedValue>) -> bool {
    let mut flag = |name: &str| props.remove(name).and_then(|v| bool::try_from(v).ok());
    flag("Active") == Some(true)
        && flag("LockedHint") != Some(true)
        && flag("IdleHint") != Some(true)
}

/// The screensaver service's word on whether the screen is locked; no service, no.
async fn screensaver_on(session: Option<&Connection>) -> bool {
    let Some(session) = session else {
        return false;
    };
    let reply = session
        .call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "GetActive",
            &(),
        )
        .await;
    reply
        .and_then(|r| r.body().deserialize::<bool>())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(
        active: bool,
        locked: Option<bool>,
        idle: Option<bool>,
    ) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        m.insert("Active".to_owned(), OwnedValue::from(active));
        if let Some(l) = locked {
            m.insert("LockedHint".to_owned(), OwnedValue::from(l));
        }
        if let Some(i) = idle {
            m.insert("IdleHint".to_owned(), OwnedValue::from(i));
        }
        m
    }

    #[test]
    fn a_session_is_in_use_while_active_unlocked_and_not_idle() {
        assert!(in_use(&mut props(true, Some(false), Some(false))));
        assert!(
            in_use(&mut props(true, None, None)),
            "a logind without the hints"
        );
        assert!(
            !in_use(&mut props(false, Some(false), Some(false))),
            "switched away"
        );
        assert!(!in_use(&mut props(true, Some(true), Some(false))), "locked");
        assert!(!in_use(&mut props(true, Some(false), Some(true))), "idle");
        assert!(!in_use(&mut HashMap::new()), "nothing known");
    }
}
