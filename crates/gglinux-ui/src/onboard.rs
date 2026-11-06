// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's onboard profile slots, from [`State::onboard`]: a panel at
//! the end of the keyboard card. Each slot shows its name, whether it checks out and its
//! newest backup, with "Load" and "Write gglinux's settings into this slot…" (which asks
//! first); "Back up all" reads and saves all five. Everything is greyed, with the reason,
//! while `keyboard_onboard` is off.
//!
//! The daemon answers each operation with the slots as they are after it, so the panel
//! never reads them again to see what an operation did.

use std::rc::Rc;
use std::time::Duration;

use gg_ipc::{
    Experimental, Feature, OnboardSlot, OnboardSource, OnboardState, Request, ONBOARD_SLOTS,
};
use slint::{ComponentHandle, SharedString};

use crate::client::{lost_connection, Client, State, TIMEOUT};
use crate::{sync_rows, AppWindow, Forms, OnboardUi};

/// A write takes about 2 s, but the erase may take up to 6, and a failed one is written
/// back the same way.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Reading all five is 125 chunk reads.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// What the panel keeps between the daemon's answers. Nothing: its state is the daemon's.
#[derive(Debug, Default)]
pub struct OnboardForm {}

/// One slot's line: its name, what's known about it, and whether it read back bad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub name: String,
    pub note: String,
    pub bad: bool,
}

/// The slots' lines, all five, slot 1 first (the daemon lists none before it knows
/// anything about them).
pub fn lines(state: &OnboardState) -> Vec<Line> {
    (0..ONBOARD_SLOTS)
        .map(|index| {
            let s = state
                .slots
                .iter()
                .find(|s| s.index == index)
                .cloned()
                .unwrap_or(OnboardSlot {
                    index,
                    ..OnboardSlot::default()
                });
            let mut notes = Vec::new();
            match s.valid {
                Some(true) => notes.push("checks out".to_owned()),
                Some(false) => notes.push("doesn't check out".to_owned()),
                None if s.backup.is_none() => notes.push("not read yet".to_owned()),
                None => {}
            }
            if let Some(path) = &s.backup {
                notes.push(match backup_time(path) {
                    Some(when) => format!("backed up {when}"),
                    None => "backed up".to_owned(),
                });
            }
            if state.active == Some(s.index) {
                notes.push("running".to_owned());
            }
            Line {
                name: s.name.clone().unwrap_or_else(|| "—".to_owned()),
                note: notes.join(" · "),
                bad: s.valid == Some(false),
            }
        })
        .collect()
}

/// When a backup was made, from its name (`slot<N>-YYYYMMDDTHHMMSSZ.bin`), as
/// `YYYY-MM-DD HH:MM UTC`.
fn backup_time(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let stamp = name.split_once('-')?.1.get(..16)?;
    let digits = |r: std::ops::Range<usize>| {
        let part = stamp.get(r)?;
        part.bytes().all(|b| b.is_ascii_digit()).then_some(part)
    };
    if stamp.as_bytes().get(8) != Some(&b'T') || stamp.as_bytes().get(15) != Some(&b'Z') {
        return None;
    }
    Some(format!(
        "{}-{}-{} {}:{} UTC",
        digits(0..4)?,
        digits(4..6)?,
        digits(6..8)?,
        digits(9..11)?,
        digits(11..13)?
    ))
}

/// Why the panel's controls don't work, if they don't: the gate is off (or unknown).
fn refusal(experimental: Option<Experimental>) -> Option<String> {
    experimental
        .unwrap_or_default()
        .refusal(Feature::KeyboardOnboard)
}

/// Show the daemon's onboard slots.
pub fn show(ui: &AppWindow, s: &State) {
    let g = ui.global::<OnboardUi>();
    let Some(state) = s.onboard.as_ref() else {
        g.set_shown(false);
        return;
    };
    g.set_shown(true);
    let refused = refusal(s.experimental);
    g.set_enabled(refused.is_none());
    g.set_refused(refused.unwrap_or_default().into());
    let lines = lines(state);
    let text =
        |f: fn(&Line) -> &str| -> Vec<SharedString> { lines.iter().map(|l| f(l).into()).collect() };
    sync_rows(g.get_names(), text(|l| l.name.as_str()), |m| g.set_names(m));
    sync_rows(g.get_notes(), text(|l| l.note.as_str()), |m| g.set_notes(m));
    sync_rows(g.get_bad(), lines.iter().map(|l| l.bad).collect(), |m| {
        g.set_bad(m)
    });
    g.set_busy(state.busy);
    if let Some(e) = &state.error {
        g.set_error(e.into());
    }
}

/// Answer the panel's buttons.
pub fn bind(ui: &AppWindow, client: &Client, _forms: &Rc<Forms>) {
    let g = ui.global::<OnboardUi>();
    let send = |make: fn(u8) -> Request, timeout: Duration| {
        let weak = ui.as_weak();
        let client = client.clone();
        move |slot: i32| {
            let Some(ui) = weak.upgrade() else { return };
            ask(&ui, &client, make(slot.clamp(0, 4) as u8), timeout);
        }
    };
    let read = send(|_| Request::ReadOnboard { slot: None }, READ_TIMEOUT);
    g.on_read_all(move || read(0));
    g.on_load(send(|slot| Request::LoadOnboard { slot }, TIMEOUT));
    g.on_write(send(
        |slot| Request::WriteOnboard {
            slot,
            from: OnboardSource::Current,
        },
        WRITE_TIMEOUT,
    ));
}

/// Send one operation: the panel is busy until the answer, which brings the slots as they
/// are after it, or the error.
fn ask(ui: &AppWindow, client: &Client, req: Request, timeout: Duration) {
    let g = ui.global::<OnboardUi>();
    g.set_busy(true);
    g.set_error(SharedString::new());
    g.set_confirm(-1);
    let weak = ui.as_weak();
    client.request(req, timeout, move |answer| {
        let error: SharedString = match answer {
            Err(e) if !lost_connection(&e) => e.into(),
            _ => SharedString::new(),
        };
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let g = ui.global::<OnboardUi>();
            g.set_busy(false);
            g.set_error(error);
        });
    });
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.onboard.borrow_mut() = OnboardForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
pub fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let g = ui.global::<OnboardUi>();
    match name {
        "onboard.read-all" => g.invoke_read_all(),
        "onboard.load" => g.invoke_load(n),
        // Slot n's write, waiting for its yes.
        "onboard.confirm" => g.set_confirm(n),
        // Everything drawn again from what the daemon said last.
        "onboard.redraw" => crate::refresh(ui),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(index: u8) -> OnboardSlot {
        OnboardSlot {
            index,
            ..OnboardSlot::default()
        }
    }

    #[test]
    fn each_slot_says_what_is_known_about_it() {
        let state = OnboardState {
            slots: vec![
                OnboardSlot {
                    name: Some("Config 1".into()),
                    valid: Some(true),
                    backup: Some(
                        "/h/.config/gglinux/onboard-backups/slot1-20261106T143456Z.bin".into(),
                    ),
                    ..slot(0)
                },
                slot(1),
                OnboardSlot {
                    valid: Some(false),
                    ..slot(2)
                },
                OnboardSlot {
                    backup: Some("/b/slot4-20261105T090000Z-2.bin".into()),
                    ..slot(3)
                },
            ],
            active: Some(1),
            ..OnboardState::default()
        };
        let l = lines(&state);
        assert_eq!(
            l[0],
            Line {
                name: "Config 1".into(),
                note: "checks out · backed up 2026-11-06 14:34 UTC".into(),
                bad: false
            }
        );
        assert_eq!(l[1].name, "—");
        assert_eq!(l[1].note, "not read yet · running");
        assert_eq!((l[2].note.as_str(), l[2].bad), ("doesn't check out", true));
        assert_eq!(
            l[3].note, "backed up 2026-11-05 09:00 UTC",
            "from an earlier run"
        );
    }

    #[test]
    fn a_backup_with_a_name_of_its_own_is_just_backed_up() {
        assert_eq!(backup_time("/x/mine.bin"), None);
        assert_eq!(backup_time("/x/slot1-notadate.bin"), None);
        assert_eq!(backup_time("/x/slot1-2026110XT143456Z.bin"), None);
    }

    #[test]
    fn the_controls_wait_for_the_gate() {
        let why = refusal(None).unwrap();
        assert!(
            why.contains("hasn't been tried on your hardware yet"),
            "{why}"
        );
        assert_eq!(refusal(Some(Experimental::default())), Some(why));
        let mut on = Experimental::default();
        on.set(Feature::KeyboardOnboard, true);
        assert_eq!(refusal(Some(on)), None);
    }
}
