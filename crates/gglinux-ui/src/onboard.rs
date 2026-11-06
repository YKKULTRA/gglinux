// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's onboard profile slots, from [`State::onboard`]: a panel at
//! the end of the keyboard card. Each slot shows its name, whether it checks out and its
//! newest backup, with "Load" and "Write gglinux's settings into this slot…" (which asks
//! first); "Back up all" reads and saves all five. Everything is greyed, with the reason,
//! while `keyboard_onboard` is off. A slot that doesn't check out gets the terminal
//! command that puts its backup back (`ggctl onboard restore N <file> --damaged`, which
//! asks its own question); the app doesn't do that itself. What the keyboard's own check
//! answered, when it wasn't `B1 00`, shows as a note.
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
/// back, up to three times.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Reading all five is 125 chunk reads, each tried up to four times; as ggctl waits.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

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
            if state.active == Some(s.index) {
                notes.push("running".to_owned());
            }
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
            Line {
                name: s.name.clone().unwrap_or_else(|| "—".to_owned()),
                note: notes.join(" · "),
                bad: s.valid == Some(false),
            }
        })
        .collect()
}

/// What to show for the slots that don't check out: the command that puts each one's
/// backup back, which the app leaves to a terminal. Empty while every slot checks out.
pub fn recovery(state: &OnboardState) -> String {
    let commands: Vec<String> = state
        .slots
        .iter()
        .filter(|s| s.valid == Some(false))
        .map(|s| {
            let file = s.backup.as_deref().unwrap_or("<backup file>");
            format!(
                "ggctl onboard restore {} {file} --damaged",
                u16::from(s.index) + 1
            )
        })
        .collect();
    if commands.is_empty() {
        return String::new();
    }
    format!(
        "A slot that doesn't check out isn't written from here. If a write to it was cut short, put its backup back from a terminal: {}",
        commands.join(" · ")
    )
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
    draw(ui, state, refusal(s.experimental));
}

/// Draw `state`, the controls refused for `refused` if it's given.
fn draw(ui: &AppWindow, state: &OnboardState, refused: Option<String>) {
    let g = ui.global::<OnboardUi>();
    g.set_shown(true);
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
    g.set_note(state.note.clone().unwrap_or_default().into());
    g.set_recovery(recovery(state).into());
}

/// For the `shot` example: slot `slot` (from 0) doesn't check out and the last write got
/// a note, as the fake daemon's slots never do, drawn with the gate on.
fn draw_damaged(ui: &AppWindow, slot: i32) {
    let bad = slot.clamp(0, 4) as u8;
    let state = OnboardState {
        slots: (0..ONBOARD_SLOTS)
            .map(|index| OnboardSlot {
                index,
                name: (index != bad).then(|| format!("Config {}", index + 1)),
                valid: Some(index != bad),
                backup: Some(format!(
                    "/home/you/.config/gglinux/onboard-backups/slot{}-20261106T120000Z.bin",
                    index + 1
                )),
            })
            .collect(),
        note: Some(
            "the keyboard's own check of the slot answered 0x01, not 0x00; what that means isn't known yet, and the slot reads back as written, so the write stands"
                .into(),
        ),
        ..OnboardState::default()
    };
    draw(ui, &state, None);
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
        // Slot n's write, waiting for its yes; then the yes.
        "onboard.confirm" => g.set_confirm(n),
        "onboard.write" => g.invoke_write(n),
        // Slot n doesn't check out, and a note: what the fake daemon never shows.
        "onboard.damaged" => draw_damaged(ui, n),
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
        assert_eq!(l[1].note, "running · not read yet");
        assert_eq!((l[2].note.as_str(), l[2].bad), ("doesn't check out", true));
        assert_eq!(
            l[3].note, "backed up 2026-11-05 09:00 UTC",
            "from an earlier run"
        );
    }

    #[test]
    fn a_slot_that_doesnt_check_out_shows_the_command_that_puts_its_backup_back() {
        let state = OnboardState {
            slots: vec![
                OnboardSlot {
                    valid: Some(true),
                    ..slot(0)
                },
                OnboardSlot {
                    valid: Some(false),
                    backup: Some("/b/slot2-20261106T143456Z.bin".into()),
                    ..slot(1)
                },
                OnboardSlot {
                    valid: Some(false),
                    ..slot(3)
                },
            ],
            ..OnboardState::default()
        };
        let text = recovery(&state);
        assert!(
            text.contains("ggctl onboard restore 2 /b/slot2-20261106T143456Z.bin --damaged"),
            "{text}"
        );
        assert!(
            text.contains("ggctl onboard restore 4 <backup file> --damaged"),
            "{text}"
        );
        assert_eq!(recovery(&OnboardState::default()), "");
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
